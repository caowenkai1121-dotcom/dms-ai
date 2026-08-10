//! 口径复核的回炉闭环。变更原因＝「违规清单 → 这一轮该做什么」的裁决。
//!
//! kernel 的 `check_caliber` 只说「哪条声明被违反」；本文件只说「所以该回炉、该收手、还是过」。
//! 两件事都**不改 SQL**——本文件里没有任何一行产 SQL 或改 AST 的代码，这是设计而不是遗漏。
//!
//! ## 三条裁决（写在这里，因为后来人的第一反应都是「为什么不直接把 SQL 改对」）
//! 1. **不静默改写**（铁律 3 / 裁决 V3）。SQLBot 用 LLM 改写 SQL 做行权限，我们明确拒绝抄那条；
//!    同理口径也不该靠盲改：改错了没人知道，回炉失败至少在 `correction_log` 留下痕迹。
//!    唯一例外是既有的保守补全 `add_scope_filter`（6 个断言守着门控），它不在本文件。
//! 2. **预算用尽 → `Unresolved` 而不是拒绝**。拒绝会让用户失去一个大概率正确的答案；
//!    而静默给数是更坏的一端——「数字错」比「没有数字」危险，因为它会被拿去做决策。
//!    折中是**照返 + 显式标注不可信 + 落 `correction_log`**，让不可信可被统计、可被回炉训练。
//! 3. **口径回炉不新开预算**：`max_rounds` 由调用方给，共用既有 repair 的 ≤2 轮。
//!    口径违规与执行失败抢同一份预算是刻意的——两者都是「这版 SQL 不能用」，没理由分家。
//!
//! `correction_log` 的 kind 由**六增至九**（新增 `caliber-retry` / `caliber-unresolved` /
//! `caliber-grader-error`）。ARCHITECTURE 那条「六个 kind 一个不少」守的是**不许少**：
//! 既有六个一个没改名、一个没删。
//!
//! ## 4. 判据自己跑挂了**不算通过**（第四态）
//! `judge` 此前只看 `violations.is_empty()`，于是「口径校验根本没跑起来」与「真的没有违规」
//! 在返回值上完全一样 —— 一次静默的假绿：`correction_log` 不留痕、答案上不留字。
//! 两种跑不起来都真实存在（判定在 `run.rs::caliber_check`）：声明取用失败（PG 抖一下），
//! 与校验器解析不动这条 SQL（`check_caliber` 对解析失败的定义就是返空清单，caliber.rs:98）。
//! 上游对照 deepagents 的 `RubricMiddleware`：它把 **grader_error 与 failed 分开**，
//! 因为「评分器炸了」和「被评分的东西不合格」要走两条不同的处置。
//! 这里的处置**与 `Unresolved` 同一条口径：照返 + 标注，不做拒绝**（裁决 二·G ——
//! 误伤一条会连带把本来对的答案回炉改错；而这一态连判都没判，更没有回炉的依据）。

use dms_kernel::Violation;

/// `correction_log.kind`：判定违规且预算未尽，本轮回炉重生成
pub const KIND_RETRY: &str = "caliber-retry";
/// `correction_log.kind`：预算用尽仍违规，结果照返但已标注不可信
pub const KIND_UNRESOLVED: &str = "caliber-unresolved";
/// `correction_log.kind`：口径判据**自己没跑起来**（第四态），这条 SQL 一次都没被校验过
pub const KIND_GRADER_ERROR: &str = "caliber-grader-error";

/// 一轮口径复核的结论。**不改写 SQL**——只说「哪条声明被违反、该怎么改」，让 LLM 自己重写。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    /// 回炉：带给 LLM 的违规清单（人话 + 修法），调用方拼进 repair prompt
    Retry(String),
    /// 预算用尽仍违规：**结果照返，但必须标注不可信**
    Unresolved(String),
    /// 🔴 判据**自己没跑起来**：既没判违规也没判通过。**绝不许与 `Pass` 同支**（见文件头 §4）。
    /// 处置同 `Unresolved`：照返 + 标注 + 落 `correction_log`（措辞不同 —— 那一态是
    /// 「查过了，数不可信」，这一态是「压根没查」，把两者说成一句话就等于没分开）。
    GraderError(String),
}

impl Verdict {
    /// 该往 `correction_log` 记哪个 kind；`Pass` 不记（通过不是一次修正）。
    /// 映射写在裁决旁边、不让调用方自己配对：配错就是几类自进化数据静默串味。
    pub fn log_kind(&self) -> Option<&'static str> {
        match self {
            Verdict::Pass => None,
            Verdict::Retry(_) => Some(KIND_RETRY),
            Verdict::Unresolved(_) => Some(KIND_UNRESOLVED),
            Verdict::GraderError(_) => Some(KIND_GRADER_ERROR),
        }
    }

    /// 结论自带的文本（`Pass` 为空）：`Retry` 的给 LLM，另两态的给用户与日志。
    /// 有它，落日志才是一行：`if let Some(k) = v.log_kind() { log_correction(k, v.detail()) }`
    pub fn detail(&self) -> &str {
        match self {
            Verdict::Pass => "",
            Verdict::Retry(s) | Verdict::Unresolved(s) | Verdict::GraderError(s) => s,
        }
    }
}

/// `round` 从 0 计（0＝首版 SQL 的第一次复核）；`max_rounds` 是调用方那份共用的 repair 预算。
///
/// 入参是 `Result` 而不是 `&[Violation]`：**「判据没跑起来」必须在类型上说得出来**。
/// 拿一个空清单当「通过」是 A1 那条静默的全部成因，而 `Err` 分支让它编译期就得被处理
/// （调用方判定见 `run.rs::caliber_check`）。用 stdlib 的 `Result` 不新造类型。
pub fn judge(check: Result<&[Violation], &str>, round: usize, max_rounds: usize) -> Verdict {
    let violations = match check {
        Ok(v) => v,
        // 第四态：不回炉（没有可执行的判词）、不拒绝、**不冒充 Pass**
        Err(why) => return Verdict::GraderError(grader_error_note(why)),
    };
    if violations.is_empty() {
        // 空清单一律 Pass。「声明缺失 ≠ 违规」已由 kernel 在宁缺毋滥一侧兜过一次，这里不再猜。
        return Verdict::Pass;
    }
    if round < max_rounds {
        Verdict::Retry(repair_instruction(violations))
    } else {
        Verdict::Unresolved(unresolved_note(violations, round))
    }
}

/// 第四态给用户的措辞。**必须与 `unresolved_note` 说的是两件事**：
/// 那一条是「校验过、没修好、数不可信」，这一条是「压根没校验过」——
/// 后者不许说成「不可信」（那是断言数字错了，而我们并不知道），只能说「未经校验」。
fn grader_error_note(why: &str) -> String {
    format!(
        "口径复核**没有跑起来**（{why}）：下方数字**未经**业务口径校验 —— \
         它既没被判违规、也没被判通过。请对照口径自行核对后再用于决策。"
    )
}

/// 回炉指令的渲染（纯函数，可单测）：一条违规一行，附「怎么改」。
/// 落点是 repair prompt 的 `## 错误` 槽位，所以开头必须自报「这不是语法错」——
/// 否则 LLM 会去找根本不存在的语法问题，把对的地方改坏。
pub fn repair_instruction(violations: &[Violation]) -> String {
    let mut s = String::from(
        "上一版 SQL 语法没问题，但违反了下列业务口径声明（口径错＝数字错，比语法错更贵）：\n",
    );
    for (i, v) in stable(violations).iter().enumerate() {
        s.push_str(&format!("{}. {} —— 改法：{}\n", i + 1, v.human, v.hint));
    }
    s.push_str("请按上述口径重写这条 SQL：只补口径，不要改变原有的查询意图、输出列与排序。");
    s
}

fn unresolved_note(violations: &[Violation], rounds: usize) -> String {
    let humans: Vec<&str> = stable(violations).iter().map(|v| v.human.as_str()).collect();
    format!(
        "口径复核未通过（{rounds} 轮回炉后仍违反 {} 条声明）：{}。\
         下方结果不可信，数字可能偏高或偏低，请勿直接用于决策。",
        humans.len(),
        humans.join("；")
    )
}

/// 按 `rule`（kernel 保证的机器可读名）排序去重。两个理由，都不是洁癖：
/// ① 声明由多个来源拼出（表级口径 + 指标注册表），同一条会被重复登记，重复行在 prompt 里是纯噪音；
/// ② 渲染顺序不再依赖调用方构造 rules 的顺序（可能来自 HashMap）→ golden 对比才稳得住。
fn stable(violations: &[Violation]) -> Vec<&Violation> {
    let mut v: Vec<&Violation> = violations.iter().collect();
    v.sort_by(|a, b| a.rule.cmp(&b.rule));
    v.dedup_by(|a, b| a.rule == b.rule);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viol(rule: &str, human: &str, hint: &str) -> Violation {
        Violation { rule: rule.into(), human: human.into(), hint: hint.into() }
    }
    fn dtl() -> Violation {
        viol("require_cols:t_sales_order_detail", "明细表标准口径", "补 item_type='1' AND deleted_flag=0")
    }
    fn snap() -> Violation {
        viol("require_latest:t_fin_balance", "快照表取最新一条", "ROW_NUMBER() 后取 rn = 1")
    }

    #[test]
    fn no_violation_passes_at_any_round() {
        assert_eq!(judge(Ok(&[]), 0, 2), Verdict::Pass);
        assert_eq!(judge(Ok(&[]), 5, 2), Verdict::Pass);
        assert_eq!(judge(Ok(&[]), 0, 2).log_kind(), None);
        assert_eq!(judge(Ok(&[]), 0, 2).detail(), "");
    }

    /// 🔴 **判据自己跑挂了 ≠ 通过**（第四态，文件头 §4）。
    ///
    /// 此前 `judge` 只看 `violations.is_empty()`，而「口径校验没跑起来」也是空清单 ——
    /// 两者在返回值上完全一样：`correction_log` 不留痕、答案上不留字、没有任何测试会红。
    /// 这条断言就是那个缝：`Err` 与 `Ok(&[])` 必须落在**不同的**支上。
    #[test]
    fn grader_error_is_not_pass_at_any_round() {
        let broken = judge(Err("口径声明取用失败：连接超时"), 0, 2);
        // ① 不是 Pass —— 反向验证过：把 `Err` 分支改成 `return Verdict::Pass` 本条当场红
        assert_ne!(broken, Verdict::Pass, "判据跑挂了不许冒充通过");
        assert!(matches!(broken, Verdict::GraderError(_)), "{broken:?}");
        // ② 必须留痕，且 kind 与另两态**不同**（串了味就统计不出「有多少答案压根没校验」）
        assert_eq!(broken.log_kind(), Some("caliber-grader-error"));
        assert_ne!(broken.log_kind(), judge(Ok(&[dtl()]), 0, 2).log_kind());
        assert_ne!(broken.log_kind(), judge(Ok(&[dtl()]), 2, 2).log_kind());
        // ③ 不回炉（没有可执行的判词，回炉只会让 LLM 去改一个没人指出问题的地方）、
        //    也不拒绝（裁决 二·G：照返 + 标注）。任何轮次都是同一支。
        for round in [0usize, 1, 5] {
            assert!(matches!(judge(Err("x"), round, 2), Verdict::GraderError(_)), "round={round}");
        }
        // ④ 措辞：说「未经校验」，**不许**说「不可信」——那是断言数字错了，而我们并不知道
        let note = broken.detail();
        assert!(note.contains("没有跑起来") && note.contains("未经"), "{note}");
        assert!(note.contains("连接超时"), "必须带上「为什么没跑起来」：{note}");
        assert!(!note.contains("不可信"), "这一态压根没校验，不许断言数字不可信：{note}");
        // 与「校验过、没修好」那一态的措辞不许撞（撞了就等于四态白分）
        assert_ne!(note, judge(Ok(&[dtl()]), 2, 2).detail());
    }

    #[test]
    fn violation_under_budget_retries_with_human_words_and_fix() {
        let v = judge(Ok(&[dtl()]), 0, 2);
        let Verdict::Retry(msg) = &v else { panic!("预算未尽必须回炉，实得 {v:?}") };
        assert!(msg.contains("明细表标准口径"), "回炉指令必须带声明的人话：{msg}");
        assert!(msg.contains("item_type='1'"), "回炉指令必须带怎么改：{msg}");
        assert!(msg.contains("语法没问题"), "必须自报不是语法错，否则 LLM 乱改：{msg}");
        assert!(msg.contains("不要改变原有的查询意图"));
        assert_eq!(v.log_kind(), Some("caliber-retry"));
        assert_eq!(v.detail(), msg);
        // 最后一轮预算仍然回炉（round 从 0 计：max_rounds=2 给足 round 0 与 round 1 两次）
        assert!(matches!(judge(Ok(&[dtl()]), 1, 2), Verdict::Retry(_)));
    }

    #[test]
    fn budget_exhausted_annotates_instead_of_refusing() {
        // 关键裁决：不拒绝、不静默——照返 + 标注不可信 + 可落 correction_log
        let v = judge(Ok(&[dtl(), snap()]), 2, 2);
        let Verdict::Unresolved(note) = &v else { panic!("预算用尽必须标注不可信，实得 {v:?}") };
        assert!(note.contains("不可信") && note.contains("请勿直接用于决策"), "{note}");
        assert!(note.contains("2 轮回炉后仍违反 2 条声明"), "{note}");
        assert!(note.contains("明细表标准口径") && note.contains("快照表取最新一条"), "{note}");
        assert_eq!(v.log_kind(), Some("caliber-unresolved"));
        // max_rounds = 0 的调用方＝只判不回炉，首轮即定案
        assert!(matches!(judge(Ok(&[dtl()]), 0, 0), Verdict::Unresolved(_)));
    }

    #[test]
    fn repair_instruction_render_is_stable_and_input_order_independent() {
        const GOLDEN: &str = "上一版 SQL 语法没问题，但违反了下列业务口径声明（口径错＝数字错，比语法错更贵）：\n\
             1. 明细表标准口径 —— 改法：补 item_type='1' AND deleted_flag=0\n\
             2. 快照表取最新一条 —— 改法：ROW_NUMBER() 后取 rn = 1\n\
             请按上述口径重写这条 SQL：只补口径，不要改变原有的查询意图、输出列与排序。";
        assert_eq!(repair_instruction(&[dtl(), snap()]), GOLDEN);
        // 调用方 rules 的构造顺序不影响渲染（rule 名排序），否则 golden 每次重拼都要改
        assert_eq!(repair_instruction(&[snap(), dtl()]), GOLDEN);
    }

    #[test]
    fn duplicate_declarations_render_once() {
        // 同一条口径被表级声明与指标声明各登记一次是常态，prompt 里重复只会稀释注意力
        let msg = repair_instruction(&[dtl(), dtl(), snap()]);
        assert_eq!(msg.matches("明细表标准口径").count(), 1, "{msg}");
        assert!(msg.contains("2. 快照表"), "去重后编号必须连续：{msg}");
    }
}
