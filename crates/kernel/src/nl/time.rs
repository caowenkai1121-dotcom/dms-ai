//! 中文时间与数量的规则解析（移植 SuperSonic TimeRangeParser 思路）。
//! 产出的一律是**列名占位为 `{}` 的谓词模板**，真实列名由调用方用 [`fill_time_col`] 填，
//! kernel 因此不含任何 DMS 列名。
//!
//! 搬运源 `server/src/direct.rs:786-990`（逐行等价）。唯一的语义等价改写：`prev_window`
//! 原先写死 `order_time`，现与 `time_predicate` 同形出模板，列名由调用方填——server 侧
//! `agg_template` 填回 `order_time`，最终 SQL 字节不变。

/// TopN 上限/默认值（＝全局 MAX_ROWS；改它三处断言与装配行数一起动）
const MAX_TOP_N: usize = 200;

/// 中文数字字符集（detect_top_n / recent_n / rule_month 四处共用）
/// 中文数字字符集（`cn_num` 的输入域）。`pub` 是给 agent 侧的序数识别用 ——
/// 抄第二份必漂（本仓为「抄了两份」付过多次账）。
pub const CN_DIGITS: &str = "零一两二三四五六七八九十";

/// 「今天/昨天」词组（prev_window / yoy_window / rule_relative 三处共用，不抄第三份）
const TODAY_WORDS: &[&str] = &["今天", "今日"];
const YESTERDAY_WORDS: &[&str] = &["昨天", "昨日"];
/// 「当月」此前只有 `rule_relative` 认，`prev_window`/`yoy_window` 不认 —— 同一个词在三处
/// 判据里两种口径：「当月销售额」算得出窗口，却拿不到「较上月」和同比两个角标。
/// 三处共用这一份（2026-08-13 审计）。
const MONTH_CUR_WORDS: &[&str] = &["本月", "这个月", "当月"];

fn contains_any(q: &str, words: &[&str]) -> bool {
    words.iter().any(|w| q.contains(w))
}

/// "前N/topN" → 限制条数（中文数字支持）。**未提则 200**（＝全局 MAX_ROWS，见函数末尾）。
/// 注释原先写「默认 50」，与实现和 `top_n_bounds` 断言都不符 —— 谁照注释改回 50，
/// 就会把 60 个商品分类静默截成 50。
pub fn detect_top_n(q: &str) -> usize {
    // "前N" / "前十"
    //
    // 🔴 数字后紧跟**时间单位**的一律不是 TopN：「前三季度」「前30天」「前两个月」是**时间窗**。
    // 修前实测：`detect_top_n("今年前三季度的金额")` = 3 —— 在一个已经错了的时间窗
    // （`rule_quarter` 出 Q3 单季）之上再叠一层「只取 3 行」，两处错各自独立。
    // 判据用**时间单位黑名单**而不是「数量单位白名单（个/名/条/项）」：后者会把既有的
    // 「前十」「金额前十的客户」（`direct.rs:1610` 钉着 =10）一起判成 200，那是新的静默截断。
    // 循环所有「前」而不是只看第一个：「…前三季度…前5的客户」里真正的 TopN 在第二个「前」上。
    for (pos, _) in q.match_indices('前') {
        let after = &q[pos + '前'.len_utf8()..];
        let number: String = after
            .chars()
            .take_while(|c| c.is_ascii_digit() || CN_DIGITS.contains(*c))
            .collect();
        let n = cn_num(&number).map(|n| n as usize);
        // 数字后面剩下的第一个字（「三个季度」的「个」不算）
        let tail = &after[number.len()..];
        let tail = tail.strip_prefix('个').unwrap_or(tail);
        let is_time = ["天", "日", "周", "星", "月", "年", "季"].iter().any(|u| tail.starts_with(u));
        match n {
            Some(n) if (1..=MAX_TOP_N).contains(&n) && !is_time => return n,
            _ => continue,
        }
    }
    // "topN"（前一个字符不许是 ASCII 字母：「stop3」「desktop5」里的 "top" 不是 TopN）
    let lower = q.to_lowercase();
    if let Some(pos) = lower.find("top") {
        let word_hit = pos == 0 || !lower.as_bytes()[pos - 1].is_ascii_alphabetic();
        let digits: String = lower[pos + 3..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if word_hit {
            if let Ok(n) = digits.parse::<usize>() {
                if (1..=MAX_TOP_N).contains(&n) {
                    return n;
                }
            }
        }
    }
    // 🔴 「最高的 N 个」/「最多的 N 个」/「最好的 N 个」—— 中文里与「前 N」完全同义的另一种说法。
    //
    // 少了这一支的代价不是「少认一种说法」，是**把一道题送进确定性路径却按 200 行出数**：
    // 「2026年6月销量最高的5个商品分类是哪些」的 gold 只要 5 行，装配器会给全部分类
    // → 行数不符、确定性地失败。也就是说**解锁一道题进确定性路径，前提是 TopN 与排序
    // 也都认得出来**；只解锁不补这个，是把「飘着的失败」换成「确定的失败」。
    //
    // 判据刻意窄：必须是「最高/最多/最大/最少/最小/最好」+（可选「的」）+ 数字/中文数字 +「个/名/条/项」。
    // 不认光秃秃的「5个」—— 那可能是「5个仓库的库存」这类**值过滤**里的数量词，
    // 按它截断就等于悄悄改了语义。
    // 「最低/最差」与上面六个同族：不认它们，「销售额最低的5个客户」「卖得最差的3个商品」
    // 的 TopN 认不出来 → 确定性路要么按 200 行截断、要么行数不符判红（2026-08-13 审计）。
    // 「倒数三名」与「最低的3个」同义（2026-08-15 生产直打：ASC+LIMIT N 的能力早就有，
    // 只是「倒数」这个说法四处词表里一处都没有 → 白拒）。
    for sup in ["最高", "最多", "最大", "最少", "最小", "最好", "最低", "最差", "倒数"] {
        // 循环**所有**出现位置（与「前」分支同形）：「最高…最好5个…」里真正带数字的
        // 可能在第二次出现才命中
        for (pos, _) in q.match_indices(sup) {
            let rest = q[pos + sup.len()..].trim_start_matches('的');
            let number: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || CN_DIGITS.contains(*c))
                .collect();
            let n = cn_num(&number).map(|n| n as usize);
            let tail = &rest[number.len()..];
            if let Some(n) = n {
                if (1..=MAX_TOP_N).contains(&n) && ["个", "名", "条", "项"].iter().any(|u| tail.starts_with(u)) {
                    return n;
                }
            }
        }
    }
    // 未提 TopN → 不截断到小值：分组基数常超 50（商品分类 60 个），
    // 截断会让"各分类销量"静默少 10 个分类且用户无感（评测抓获）。对齐全局 MAX_ROWS。
    MAX_TOP_N
}

/// 时间窗 → 上一期谓词模板 + 环比标签（列名占位 `{}`，与 `time_predicate` 同形）
///
/// 🔴 **上期与当期必须共用同一个「今天」锚点**，否则出的是错数而不是缺功能。
/// 修前：「本月」的上期是 `>= 上月初 AND < 本月初` ＝ **整个上月**，而当期
/// `>= 月初 AND < 下月初` 实际只有**至今**的数据；「今年」那档是**去年整年** vs 年初至今。
/// `semantic::present::patch_kpi_delta` 拿这两个数直接 `(cur-prev)/prev*100` 塞进
/// `items[].delta`，前端照显示「较上月 -87%」。按天数就能看出与业务无关：7 月 2 日那天
/// 当期 2 天 / 上期 30 天 ⇒ 环比恒 ≈ -93%，月初越靠前越夸张。
///
/// 逐档核过语义，只有「至今才有数据」的那三档改了右端：
/// - 「今天/昨天」：单日，两边都是完整一天 —— 不动。
/// - 「上月」：当期本身就是**完整**的上月，上期该是完整的上上月 —— 不动。
/// - 「本月/本周/今年」：当期是「期初至今」，上期右端因此必须是「今天平移一期」。
pub fn prev_window(q: &str) -> Option<(&'static str, &'static str)> {
    if contains_any(q, TODAY_WORDS) {
        Some(("DATE({}) = CURDATE() - INTERVAL 1 DAY", "较昨天"))
    } else if contains_any(q, YESTERDAY_WORDS) {
        Some(("DATE({}) = CURDATE() - INTERVAL 2 DAY", "较前天"))
    } else if contains_any(q, MONTH_CUR_WORDS) {
        // 右端 `CURDATE() - INTERVAL 1 MONTH` 存在月末压缩（3/31→2/28：当期 31 天 vs 上期
        // 28 天）—— 同进度比较下这一折中接受（逐档核过见函数文档）
        Some(("{} >= DATE_FORMAT(CURDATE() - INTERVAL 1 MONTH,'%Y-%m-01') AND {} < CURDATE() - INTERVAL 1 MONTH", "较上月"))
    } else if q.contains("上月") || q.contains("上个月") {
        Some(("{} >= DATE_FORMAT(CURDATE() - INTERVAL 2 MONTH,'%Y-%m-01') AND {} < DATE_FORMAT(CURDATE() - INTERVAL 1 MONTH,'%Y-%m-01')", "较上上月"))
    } else if q.contains("本周") || q.contains("这周") {
        // YEARWEEK 等式给左端（周一起），右端再切到「今天 - 7 天」——否则是上周整周 vs 本周至今
        Some(("YEARWEEK({}, 1) = YEARWEEK(CURDATE() - INTERVAL 7 DAY, 1) AND {} < CURDATE() - INTERVAL 7 DAY", "较上周"))
    } else if q.contains("今年") {
        Some(("{} >= DATE_FORMAT(CURDATE() - INTERVAL 1 YEAR,'%Y-01-01') AND {} < CURDATE() - INTERVAL 1 YEAR", "较去年"))
    } else {
        None
    }
}

/// 相对时间词 → MySQL 谓词（基于 CURDATE()，零硬编码年份）
/// 中文数字 → 阿拉伯数字（仅覆盖 1~99，够用于「近三个月」「第二季度」这类问法）。
///
/// `pub`：`agent::answer_contract` 的事实核验也要它 —— 此前那边把**任何**中文数字
/// 一律判「暂不能精确核验」，于是模型写一句「约三家客户」整篇 AI 文案就被丢掉，
/// 而能精确换算的转换器一直躺在这里。仓里只许有一份中文数字换算。
pub fn cn_num(s: &str) -> Option<u32> {
    const D: &[(char, u32)] = &[
        ('零', 0), ('一', 1), ('两', 2), ('二', 2), ('三', 3), ('四', 4),
        ('五', 5), ('六', 6), ('七', 7), ('八', 8), ('九', 9),
    ];
    if let Ok(n) = s.parse::<u32>() {
        return Some(n);
    }
    let c: Vec<char> = s.chars().collect();
    let val = |x: char| D.iter().find(|(k, _)| *k == x).map(|(_, v)| *v);
    match c.as_slice() {
        ['十'] => Some(10),
        [a] => val(*a),
        ['十', b] => val(*b).map(|v| 10 + v),               // 十二
        [a, '十'] => val(*a).map(|v| v * 10),                // 三十
        [a, '十', b] => Some(val(*a)? * 10 + val(*b)?),       // 三十五
        _ => None,
    }
}

/// 抽「近/过去/最近 N 天|周|月|年」里的 N 与单位
fn recent_n(q: &str) -> Option<(u32, &'static str)> {
    for lead in ["最近", "过去", "近"] {
        // 循环该 lead 的**所有**出现位置：「最近销量，近7天呢」里「最近」后无数字时，
        // 句尾的「近7天」仍要轮得到（`find` 只看第一次会把整条规则弃权）
        for (pos, _) in q.match_indices(lead) {
            // take(6) 的窗口上界：最长形态「三十五」+「个月」= 4 字，6 是带余量的截取
            let rest: String = q[pos + lead.len()..].chars().take(6).collect();
            let num: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || CN_DIGITS.contains(*c))
                .collect();
            if num.is_empty() {
                continue;
            }
            let Some(n) = cn_num(&num) else { continue };
            // 「个周/个月」先剥「个」（与 detect_top_n 的同款处理）
            let tail = rest[num.len()..].strip_prefix('个').unwrap_or(&rest[num.len()..]);
            let unit = if tail.starts_with('天') || tail.starts_with('日') {
                "DAY"
            } else if tail.starts_with('周') || tail.starts_with('星') {
                "WEEK"
            } else if tail.starts_with('月') {
                "MONTH"
            } else if tail.starts_with('年') {
                "YEAR"
            } else {
                continue;
            };
            if (1..=60).contains(&n) {
                return Some((n, unit));
            }
        }
    }
    None
}

/// 规则时间解析（移植 SuperSonic TimeRangeParser 思路）：问句 → 半开区间 [起, 止)。
/// 返回的是**列名占位为 `{}` 的谓词模板**，调用方填真实时间列。
/// 时间是 BI 最高频错误源；能规则解析的一律不交给 LLM 猜。
///
/// **六条规则的先后顺序即行为**（§0 D9）：近 N 单位 → 季度 → 半年 → N 月 → 年份 → 相对词兜底。
/// 例：「上个月」必须被相对词兜底接走，若 N 月规则先跑就会把「个月」误解成 N 月；
/// `rule_year`（规则④·5）必须排在月/季度之后（否则「2025年6月」被吞成整年）。
pub fn time_predicate(q: &str) -> Option<String> {
    rule_date_range(q)
        .or_else(|| rule_recent_n(q))
        .or_else(|| rule_quarter(q))
        .or_else(|| rule_half_year(q))
        .or_else(|| rule_month(q))
        // 「只给年份」必须排在月/季度/半年**之后**：否则「2025年6月」会被吞成整年 2025。
        .or_else(|| rule_year(q))
        .or_else(|| rule_relative(q))
}

/// 当期时间窗 → 去年同期谓词模板。只覆盖能够做严格同进度比较的高频经营周期；
/// 其它任意区间宁可不展示同比，也不把不同长度的窗口相除。
pub fn yoy_window(q: &str) -> Option<(&'static str, &'static str)> {
    if contains_any(q, TODAY_WORDS) {
        Some(("DATE({}) = CURDATE() - INTERVAL 1 YEAR", "同比"))
    } else if contains_any(q, YESTERDAY_WORDS) {
        Some(("DATE({}) = CURDATE() - INTERVAL 1 YEAR - INTERVAL 1 DAY", "同比"))
    } else if contains_any(q, MONTH_CUR_WORDS) {
        Some(("{} >= DATE_FORMAT(CURDATE() - INTERVAL 1 YEAR,'%Y-%m-01') AND {} < CURDATE() - INTERVAL 1 YEAR", "同比"))
    } else if q.contains("上月") || q.contains("上个月") {
        Some(("{} >= DATE_FORMAT(CURDATE() - INTERVAL 1 YEAR - INTERVAL 1 MONTH,'%Y-%m-01') AND {} < DATE_FORMAT(CURDATE() - INTERVAL 1 YEAR,'%Y-%m-01')", "同比"))
    } else if q.contains("本周") || q.contains("这周") {
        // 52 周平移保持星期结构一致，适合周经营比较。
        Some(("YEARWEEK({}, 1) = YEARWEEK(CURDATE() - INTERVAL 364 DAY, 1) AND {} < CURDATE() - INTERVAL 364 DAY", "同比"))
    } else {
        None
    }
}

/// 显式 ISO 日期范围（`YYYY-MM-DD 至 YYYY-MM-DD` / `到` / `~`）。
/// 右端按自然语言的“截至某日”解释为**含当日**，SQL 用半开区间 `< DATE_ADD(end, 1 DAY)`。
/// 这里只接受 20xx 年的真实公历日期，拒绝把任意数字串拼进 SQL。
fn rule_date_range(q: &str) -> Option<String> {
    let range = explicit_iso_date_range(q)?;
    Some(format!(
        "{{}} >= '{}' AND {{}} < DATE_ADD('{}', INTERVAL 1 DAY)",
        range.start, range.end,
    ))
}

struct IsoDateRange {
    start_at: usize,
    end_at: usize,
    start: String,
    end: String,
}

fn explicit_iso_date_range(q: &str) -> Option<IsoDateRange> {
    let mut dates = Vec::new();
    // 按字节窗口找纯 ASCII 日期：不会在中文 UTF-8 的中间字节上切 `str`，也不会因
    // 第一个非字符边界就提前退出整条规则。
    for (at, b) in q.as_bytes().windows(10).enumerate() {
        let Some(date) = valid_iso_date(b) else { continue };
        let before = at.checked_sub(1).and_then(|i| q.as_bytes().get(i));
        let after = q.as_bytes().get(at + 10);
        if before.is_some_and(u8::is_ascii_digit) || after.is_some_and(u8::is_ascii_digit) {
            continue;
        }
        let text = std::str::from_utf8(b).expect("ISO 日期窗口已验证为 ASCII").to_string();
        dates.push((at, text, date));
        if dates.len() == 2 {
            break;
        }
    }
    let [(start_at, start, start_date), (end_at, end, end_date)] = dates.as_slice() else {
        return None;
    };
    let separator = q.get(start_at + 10..*end_at)?;
    if separator.trim().is_empty()
        || !separator.chars().all(|c| c.is_whitespace() || "至到~～-—–".contains(c))
        || start_date > end_date
    {
        return None;
    }
    Some(IsoDateRange {
        start_at: *start_at,
        end_at: end_at + 10,
        start: start.clone(),
        end: end.clone(),
    })
}

/// 从问句中移除一段**已验证的**显式 ISO 日期范围，供残留守卫消化时间限定。
/// 单个日期、非法日期或日期间不是范围连接符时返回 `None`；不会全局吞掉实体名里的“至”。
pub fn strip_explicit_date_range(q: &str) -> Option<String> {
    let range = explicit_iso_date_range(q)?;
    Some(format!("{}{}", &q[..range.start_at], &q[range.end_at..]))
}

fn valid_iso_date(b: &[u8]) -> Option<(u16, u8, u8)> {
    if b.len() != 10
        || b[0] != b'2'
        || b[1] != b'0'
        || !b[2].is_ascii_digit()
        || !b[3].is_ascii_digit()
        || b[4] != b'-'
        || !b[5].is_ascii_digit()
        || !b[6].is_ascii_digit()
        || b[7] != b'-'
        || !b[8].is_ascii_digit()
        || !b[9].is_ascii_digit()
    {
        return None;
    }
    let year = 2000 + u16::from(b[2] - b'0') * 10 + u16::from(b[3] - b'0');
    let month = (b[5] - b'0') * 10 + (b[6] - b'0');
    let day = (b[8] - b'0') * 10 + (b[9] - b'0');
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    (day >= 1 && day <= max_day).then_some((year, month, day))
}

/// 规则①：近 N 天/周/月/年（含中文数字）
fn rule_recent_n(q: &str) -> Option<String> {
    let (n, unit) = recent_n(q)?;
    // 🔴 上界含今天（`< 明天`），所以下界必须**多回推一天再往前挪一天**，窗口才恰好 N 个单位。
    //
    // 「近 N 天」早就这么修了（回推 N-1 天）。周/月/年当时按「滚动周期单位，不折算」放过，
    // 于是同一个用户意图两个窗口（2026-08-15 并行猎捕实测）：
    //   近7天   → 2026-08-09..08-15 共 7 天  ✅
    //   近一周  → 2026-08-08..08-15 共 8 天  ❌（多算一天）
    // 「近一周」和「近7天」是同一句话的两种说法，不该给两个数。
    // 周/月/年改成「先回推 N 个单位、再 +1 天」——日历算术自己处理月末长度，
    // 不用把月折算成天（那会在 1/31 → 2/28 上出错）。
    debug_assert!(n >= 1, "recent_n 的 1..=60 过滤保证 n-1 不下溢");
    let lower = if unit == "DAY" {
        format!("DATE_SUB(CURDATE(), INTERVAL {} DAY)", n - 1)
    } else {
        format!("DATE_ADD(DATE_SUB(CURDATE(), INTERVAL {n} {unit}), INTERVAL 1 DAY)")
    };
    Some(format!(
        "{{}} >= {lower} AND {{}} < DATE_ADD(CURDATE(), INTERVAL 1 DAY)"
    ))
}

/// 规则②：第 N 季度 / 本季度 / 上季度
fn rule_quarter(q: &str) -> Option<String> {
    let pos = q.find("季度")?;
    // 「季度」前最后 3 个字（先收集再切尾部，不做双重反转收集）
    let all: Vec<char> = q[..pos].chars().collect();
    let head: String = all[all.len().saturating_sub(3)..].iter().collect();
    // 序数两种来源语义不一（刻意）：中文支取数组里**最先**出现（position），数字支取 head
    // 里**最后一个**数字（rev().find）—— head 同时含两种序数时取谁取决于类型而非位置，
    // 实测形态没踩过这条缝，先注释钉住而不是统一（统一要过装配回归）。
    let qn = ["一", "二", "三", "四"]
        .iter()
        .position(|c| head.contains(c))
        .map(|i| i as u32 + 1)
        .or_else(|| head.chars().rev().find(|c| ('1'..='4').contains(c)).and_then(|c| c.to_digit(10)));
    // 序数**前面紧挨着的那个词**决定语义，修前它被完全忽略：「今年前三季度」的 head 是
    // 「年前三」，只在里头找序数「三」→ start_month=7 → 出 **7-9 月单季**，而用户问的是
    // 1-9 月。取 `q[..pos]` 而不是 3 字的 head：head 会把「过去」截成「去」。
    // 末尾的「个」先剥（「前三个季度」＝「前三季度」）。
    let pre: Vec<char> = q[..pos].chars().collect();
    let pre = if pre.last() == Some(&'个') { &pre[..pre.len() - 1] } else { &pre[..] };
    let lead: String = match pre.split_last() {
        Some((c, rest)) if "一二三四1234".contains(*c) => rest.iter().collect(),
        _ => String::new(),
    };
    if let Some(n) = qn {
        // 「近/最近/过去 N 季度」是**滚动窗**（起点随今天漂，且到底含不含本季度没有定论）
        // → 交回 LLM，不臆造窗口。修前它们出的是「第 N 季度」，纯属把序数认错。
        if lead.ends_with('近') || lead.ends_with("过去") {
            return None;
        }
        // 「前 N 季度」＝ Q1..QN（年初 → 第 N 季度末），中文财报口径，不是第 N 单季。
        if lead.ends_with('前') {
            let b = year_base(q);
            return Some(format!("{{}} >= {} AND {{}} < {}", ym(&b, 1), ym(&b, n * 3 + 1)));
        }
        let start_month = (n - 1) * 3 + 1;
        // 显式年份走字面日期；无显式年份时**保持原来的 CONCAT(YEAR(CURDATE()),…) 字节**
        // （既有断言钉着 `-04-01` 这类子串），只是右端改用 `ym` 统一表达半开区间。
        if let YearBase::Explicit(_) | YearBase::LastYear = year_base(q) {
            let b = year_base(q);
            return Some(format!(
                "{{}} >= {} AND {{}} < {}",
                ym(&b, start_month),
                ym(&b, start_month + 3)
            ));
        }
        return Some(format!(
            "{{}} >= DATE_FORMAT(CONCAT(YEAR(CURDATE()),'-{start_month:02}-01'),'%Y-%m-%d') \
             AND {{}} < DATE_ADD(DATE_FORMAT(CONCAT(YEAR(CURDATE()),'-{start_month:02}-01'),'%Y-%m-%d'), INTERVAL 3 MONTH)"
        ));
    }
    if head.contains('本') || head.contains('这') {
        return Some("QUARTER({}) = QUARTER(CURDATE()) AND YEAR({}) = YEAR(CURDATE())".into());
    }
    if head.contains('上') {
        return Some(
            "{} >= DATE_SUB(MAKEDATE(YEAR(CURDATE()),1) + INTERVAL QUARTER(CURDATE())*3-3 MONTH, INTERVAL 3 MONTH) \
             AND {} < MAKEDATE(YEAR(CURDATE()),1) + INTERVAL QUARTER(CURDATE())*3-3 MONTH"
                .into(),
        );
    }
    None
}

/// 问句里的**显式四位年份**（`2025年` 形态，2000..=2099）。要求紧跟 `年` 字：
/// 否则 `1032`（公司编码）这类四位数字会被当年份。
///
/// 🔴 它存在的理由是个实测缺陷：`上半年 / 第N季度 / N月` 三条规则原先都写死
/// `CURDATE()` 的年份，**完全忽略问句里的显式年份**。于是「2025年上半年销量」会算出
/// **2026 年**上半年，而这个错窗口是当作「已按问句规则解析，直接照用」写进 prompt 的
/// —— 也就是把错的口径当权威交给 LLM。评测里 GOODS13 问的是「2026年上半年」，
/// **只因恰好等于今年**才一直没暴露。
/// `pub` 的第二个消费者是**残留守卫**（`server::direct::has_entity_residue`）：
/// 装配器会把时间窗装进 WHERE，也就是说显式年份**确实被消化了**，
/// 而通用虚词表 `STRIP_WORDS` 认不出阿拉伯数字 → 「2026年6月动销商品」被判成有实义残留、
/// 整条回落 LLM。让守卫消化「时间解析真的认下的那个年份」，比往虚词表里塞任意数字串安全得多
/// —— 后者会把实体编码（`1032` 公司码）也剥掉，那正是 E16 那条防线要挡的东西。
pub fn explicit_year(q: &str) -> Option<i32> {
    let cs: Vec<char> = q.chars().collect();
    for (i, c) in cs.iter().enumerate() {
        if *c != '年' {
            continue;
        }
        let mut j = i;
        while j > 0 && cs[j - 1].is_ascii_digit() {
            j -= 1;
        }
        if i - j != 4 {
            continue;
        }
        // 四位 ASCII 数字直接手算（不再为 4 个字堆分配一个 String 去 parse）
        let y = (cs[j] as i32 - 48) * 1000
            + (cs[j + 1] as i32 - 48) * 100
            + (cs[j + 2] as i32 - 48) * 10
            + (cs[j + 3] as i32 - 48);
        if (2000..=2099).contains(&y) {
            return Some(y);
        }
    }
    None
}

/// 「哪一年」的三种来源。`ThisYear` 那一支**输出字节与本枚举引入前逐字相同**
/// （既有 golden 断言钉着它），所以只有显式年份与「去年」是新增行为。
enum YearBase {
    Explicit(i32),
    LastYear,
    ThisYear,
}

fn year_base(q: &str) -> YearBase {
    match explicit_year(q) {
        Some(y) => YearBase::Explicit(y),
        // 「去年上半年」此前也被算成今年上半年（同一个缺陷的另一面）
        None if q.contains("去年") => YearBase::LastYear,
        None => YearBase::ThisYear,
    }
}

/// `年-月-01` 的 SQL 表达式。`month` 允许 13（＝次年 1 月），供半开区间的右端用。
fn ym(base: &YearBase, month: u32) -> String {
    debug_assert!((1..=13).contains(&month), "month=0 或 14+ 会生成非法 SQL（'%Y-00-01'）");
    let (yshift, m) = if month > 12 { (1, month - 12) } else { (0, month) };
    match base {
        YearBase::Explicit(y) => format!("'{}-{m:02}-01'", y + yshift),
        YearBase::LastYear => format!(
            "DATE_FORMAT(DATE_SUB(CURDATE(), INTERVAL {} YEAR),'%Y-{m:02}-01')",
            1 - yshift
        ),
        YearBase::ThisYear if yshift == 1 => {
            format!("DATE_FORMAT(DATE_ADD(CURDATE(), INTERVAL 1 YEAR),'%Y-{m:02}-01')")
        }
        YearBase::ThisYear => format!("DATE_FORMAT(CURDATE(),'%Y-{m:02}-01')"),
    }
}

/// 规则③：上半年 / 下半年（年份取 `year_base`）
fn rule_half_year(q: &str) -> Option<String> {
    let b = year_base(q);
    if q.contains("上半年") {
        return Some(format!("{{}} >= {} AND {{}} < {}", ym(&b, 1), ym(&b, 7)));
    }
    if q.contains("下半年") {
        return Some(format!("{{}} >= {} AND {{}} < {}", ym(&b, 7), ym(&b, 13)));
    }
    None
}

/// 规则④：N 月 / N 月份（本年度；「上个月」等相对词在规则⑤兜底，先排除）
fn rule_month(q: &str) -> Option<String> {
    // 「个月」同时挡住「前五个月/哪个月」这类没有任何规则承接的说法：一律交兜底/LLM
    // （保守 None，不臆造窗口）
    if q.contains("个月") || q.contains("上月") {
        return None;
    }
    let pos = q.find('月')?;
    // 🔴 数字必须**紧贴**「月」：中间隔着别的字就不是「N月」（2026-08-15 生产直打）。
    //
    // 旧写法取「月」前两个字符再 `filter` 掉非数字 —— 于是「180135本月销售额」里的
    // 「5本」被滤成「5」，整句被读成**5 月**；「客户180157本月销售额」读成 7 月
    // （N ＝ 客户编码的最后一位）。用户问本月，拿到的是今年 N 月至今的累计数，
    // 答案里没有任何提示。这是纯数字客户编码那族错答的根因之一。
    //
    // 改成从「月」往前**连续**取数字，遇到非数字立刻停：
    //   「6月」「2026年6月」「十二月」照旧命中；「…5本月」不再命中，交给相对词兜底（本月）。
    let mut digits: Vec<char> = Vec::new();
    for c in q[..pos].chars().rev().take(2) {
        if c.is_ascii_digit() || CN_DIGITS.contains(c) {
            digits.push(c);
        } else {
            break;
        }
    }
    digits.reverse();
    let num: String = digits.into_iter().collect();
    let m = cn_num(&num).filter(|m| (1..=12).contains(m))?;
    // 同 `rule_quarter`：显式年份/去年走字面日期，今年那一支字节不变
    if let YearBase::Explicit(_) | YearBase::LastYear = year_base(q) {
        let b = year_base(q);
        return Some(format!("{{}} >= {} AND {{}} < {}", ym(&b, m), ym(&b, m + 1)));
    }
    Some(format!(
        "{{}} >= DATE_FORMAT(CONCAT(YEAR(CURDATE()),'-{m:02}-01'),'%Y-%m-%d') \
         AND {{}} < DATE_ADD(DATE_FORMAT(CONCAT(YEAR(CURDATE()),'-{m:02}-01'),'%Y-%m-%d'), INTERVAL 1 MONTH)"
    ))
}

/// 规则④·5：**只给了年份**（`2025年金额`）。必须排在 `rule_month`/`rule_quarter`/
/// `rule_half_year` 之后（否则「2025年6月」会被吞成整年），排在相对词兜底之前无所谓
/// （`今年/去年` 不含四位数字，两者不会互相抢）。
fn rule_year(q: &str) -> Option<String> {
    let y = explicit_year(q)?;
    Some(format!("YEAR({{}}) = {y}"))
}

/// 规则⑤：相对词兜底
fn rule_relative(q: &str) -> Option<String> {
    let p = if contains_any(q, TODAY_WORDS) {
        "DATE({}) = CURDATE()"
    } else if contains_any(q, YESTERDAY_WORDS) {
        "DATE({}) = CURDATE() - INTERVAL 1 DAY"
    } else if q.contains("前天") {
        "DATE({}) = CURDATE() - INTERVAL 2 DAY"
    } else if contains_any(q, MONTH_CUR_WORDS) {
        "{} >= DATE_FORMAT(CURDATE(),'%Y-%m-01') AND {} < DATE_ADD(DATE_FORMAT(CURDATE(),'%Y-%m-01'), INTERVAL 1 MONTH)"
    } else if q.contains("上月") || q.contains("上个月") {
        "{} >= DATE_FORMAT(CURDATE() - INTERVAL 1 MONTH,'%Y-%m-01') AND {} < DATE_FORMAT(CURDATE(),'%Y-%m-01')"
    } else if q.contains("本周") || q.contains("这周") {
        "YEARWEEK({}, 1) = YEARWEEK(CURDATE(), 1)"
    } else if q.contains("上周") {
        "YEARWEEK({}, 1) = YEARWEEK(CURDATE() - INTERVAL 1 WEEK, 1)"
    } else if q.contains("今年") || q.contains("本年") || q.contains("年初至今") {
        "YEAR({}) = YEAR(CURDATE())"
    } else if q.contains("去年") {
        "YEAR({}) = YEAR(CURDATE()) - 1"
    } else {
        return None;
    };
    Some(p.to_string())
}

/// 谓词模板填入真实时间列
pub fn fill_time_col(tpl: &str, col: &str) -> String {
    tpl.replace("{}", col)
}

/// 把窗口的右端改成「到昨天」：追加 `AND {} < CURDATE()`（`meta.metric.time_cap='yesterday'`
/// 的指标专用，如发货净销售额 —— 发货数据当天不全，含今天实测虚 1.8%）。
///
/// 追加而不是改原窗口：① 期窗（本月/今年）的**左端**仍由模板给出，只有右端要压；
/// ② 已经排除今天的窗口（昨天/上月/去年）追加后是冗余条件，语义不变形
/// （`x < 上月初 AND x < CURDATE()` 仍是上月）—— 比起按模板形态分别处理，追加零分支。
pub fn cap_at_yesterday(tpl: &str) -> String {
    format!("{tpl} AND {{}} < CURDATE()")
}

/// 问句的时间词是不是「当期」（窗口含今天）：`RequireTimeCap` 的构造闸 ——
/// 只在当期问法时要求「到昨天」的上限；问「上月/去年」时那条件本就不必出现，
/// 判了就是误伤（与 `drop_conflicting_time_cols` 同一条宁缺毋滥）。
/// 词表与 `rule_relative` 的当期分支一一对应 + 「最近」（无数字也算当期，金标钉着）；
/// 单字「近」走 `recent_n` 判定（近三个月/近7天都含今天，而「附近的门店」「接近」里的
/// 「近」不是时间词 —— 单字「近」曾把它们误判成当期 → `RequireTimeCap` 误造闸）。
pub fn window_includes_today(q: &str) -> bool {
    const CURRENT: &[&str] =
        &["今天", "今日", "本月", "这个月", "当月", "本周", "这周", "今年", "本年", "年初至今", "最近"];
    CURRENT.iter().any(|w| q.contains(w)) || recent_n(q).is_some()
}

/// 问句里**词表序首个**时间表面词（【A17 ①】日期继承用）：上一轮问句有、改写后丢了时，
/// 把这个词原样接到新问题尾巴（「那品类第二的呢」→「那品类第二的呢，上月」）。
/// 注意是**词表序**（最长最具体优先，「上个月」先于「上月」）而不是句中位置序：
/// 「本月销量比上月」返回的是「上月」（表序在前）—— 行为钉在这里，别按字面改成位置序。
/// 只继承唯一无歧义形态（「近三个月」）：recent_n 支持 1~60 全档，但其它「近 N」
/// 形态不继承是刻意的保守。带显式年份（`20xx`）的一律 `None`：继承「2025年上半年」
/// 到明年是静默改年份。
pub fn time_phrase_of(q: &str) -> Option<&'static str> {
    if q.contains("20") {
        return None;
    }
    // 🔴 季度族必须在这里（2026-08-15 生产直打）：`time_predicate` 早就认得
    // 「上个季度/本季度」，而本函数返 None → `intent_time_surface` 拿不到表面词 →
    // 残留守卫看到一个孤零零的「上」就把整条拒掉，用户收到
    // 「问句含未能识别的限定「上」」。谓词认得、消化词不认得 = 白拒一族常见问法。
    // 顺序：长形态在前（`find` 取第一个命中），`上个季度` 必须先于 `上季度`。
    // 🔴 **长形态必须在前**，否则短形态会把长形态吃掉半截（2026-08-15 生产直打）：
    // `find` 取第一个 `contains` 命中，而消化词只记这一条 ——
    //   「最近三个月销售额」命中「近三个月」→ 只消化 4 个字 → 残留孤字「最」→ 整条拒答；
    //   「今年以来销售额」  命中「今年」    → 残留「以来」→ 同样拒答。
    // 这是 aa3ec14（把已识别时间表面词并进消化词）带来的回归：在那之前这一族靠
    // `lexicon::STRIP_WORDS` 剥（那张表里「最近」排在单字「近」之前，剥得干净），
    // 新捷径抢了先。补齐长形态即可，`quarter_phrases_have_both_a_surface_and_a_predicate`
    // 会钉住「表里每一条都必须有谓词」，不会变成静默丢限定。
    const PHRASES: &[&str] = &[
        "最近三个月", "过去三个月", "近三个月",
        "上个季度", "这个季度", "上季度", "本季度",
        "今年以来", "年初至今",
        "上个月", "上周", "本周", "这周", "上月", "本月", "这个月", "当月",
        "去年", "前年", "今年", "本年", "昨天", "今日", "今天", "前天", "上半年", "下半年",
    ];
    PHRASES.iter().find(|w| q.contains(**w)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tp(q: &str) -> String {
        time_predicate(q).unwrap_or_else(|| panic!("未解析: {q}"))
    }

    /// 季度族必须**同时**有表面词与谓词。
    ///
    /// 🔴 两条都要（2026-08-15 生产直打）：
    /// - 只有谓词没表面词 → `intent_time_surface` 拿不到词 → 残留守卫看到孤零零的「上」
    ///   把整条拒掉（用户收到「问句含未能识别的限定「上」」）；
    /// - 只有表面词没谓词 → 那个词被「消化」掉却没兑现成时间窗，等于**静默丢限定**
    ///   （`当季度` 就是这一档，故意不收）。
    #[test]
    fn quarter_phrases_have_both_a_surface_and_a_predicate() {
        for q in ["上个季度销售额", "上季度销售额", "本季度销售额", "这个季度销售额"] {
            let phrase = time_phrase_of(q).unwrap_or_else(|| panic!("{q} 缺表面词"));
            assert!(q.contains(phrase), "{q} 的表面词 {phrase} 不在原句里");
            assert!(time_predicate(phrase).is_some(), "{q} 的表面词 {phrase} 没有谓词 → 会静默丢限定");
        }
        // 表面词表里的每一条都必须有谓词（新增时这条会红）
        for q in [
            "最近三个月", "过去三个月", "近三个月", "今年以来", "年初至今",
            "上个月", "上周", "本周", "上月", "本月", "去年", "今年", "昨天", "上半年",
        ] {
            assert!(time_predicate(q).is_some(), "{q} 在表面词表里却没有谓词");
        }
        // 🔴 长形态必须先命中：短形态吃掉半截会留下孤字残留，整条问句被拒
        // （「最近三个月」→ 残留「最」；「今年以来」→ 残留「以来」）
        for (q, want) in [
            ("最近三个月销售额", "最近三个月"),
            ("过去三个月销售额", "过去三个月"),
            ("今年以来销售额", "今年以来"),
            ("年初至今销售额", "年初至今"),
        ] {
            assert_eq!(time_phrase_of(q), Some(want), "{q} 该整段命中");
        }
    }

    /// 数字必须紧贴「月」才算 `N月`。
    ///
    /// 🔴 由来（2026-08-15 生产直打）：旧写法取「月」前两字符再滤掉非数字，
    /// 「180135本月销售额」的「5本」被滤成「5」→ 整句读成**5 月**；
    /// 「客户180157本月销售额」→ 7 月（N ＝ 客户编码末位）。
    /// 用户问本月，拿到今年 N 月至今的累计数，且答案里没有任何提示。
    #[test]
    fn digits_must_touch_the_month_character() {
        for (q, want) in [
            ("180135本月销售额", "DATE_FORMAT(CURDATE(),'%Y-%m-01')"),
            ("客户180157本月销售额", "DATE_FORMAT(CURDATE(),'%Y-%m-01')"),
            ("客户编码180135的本月销售额", "DATE_FORMAT(CURDATE(),'%Y-%m-01')"),
        ] {
            assert!(tp(q).contains(want), "{q} 该按本月算：{}", tp(q));
        }
        // 真的写了 N月 的照旧命中
        assert!(tp("6月销售额").contains("'-06-01'"), "{}", tp("6月销售额"));
        assert_eq!(tp("2025年6月"), "{} >= '2025-06-01' AND {} < '2025-07-01'");
        assert!(tp("十二月销售额").contains("'-12-01'"), "{}", tp("十二月销售额"));
    }

    /// 🔴 显式年份必须被尊重。缺陷现场：`上半年/第N季度/N月` 三条规则都写死 `CURDATE()` 的年份，
    /// 于是「2025年上半年」算出**今年**上半年 —— 而这个窗口是当作
    /// 「已按问句规则解析，直接照用」写进 prompt 的，等于把错口径当权威交给 LLM。
    /// 评测 GOODS13 问的是「2026年上半年」，**只因恰好等于今年**才一直没暴露。
    #[test]
    fn explicit_year_is_respected_not_silently_replaced_by_this_year() {
        // 半年
        assert_eq!(tp("2025年上半年"), "{} >= '2025-01-01' AND {} < '2025-07-01'");
        assert_eq!(tp("2023年下半年"), "{} >= '2023-07-01' AND {} < '2024-01-01'");
        // 季度（跨年右端）
        assert_eq!(tp("2024年第四季度"), "{} >= '2024-10-01' AND {} < '2025-01-01'");
        assert_eq!(tp("2024年第二季度"), "{} >= '2024-04-01' AND {} < '2024-07-01'");
        // N 月（跨年右端）
        assert_eq!(tp("2022年12月"), "{} >= '2022-12-01' AND {} < '2023-01-01'");
        assert_eq!(tp("2025年6月"), "{} >= '2025-06-01' AND {} < '2025-07-01'");
        // 只给年份 → 整年；且必须**排在月/季度之后**（否则上面那条会被吞成整年）
        assert_eq!(tp("2025年的数"), "YEAR({}) = 2025");
        // 「去年上半年」此前也被算成今年上半年（同一缺陷的另一面）
        assert!(tp("去年上半年").contains("INTERVAL 1 YEAR"), "{}", tp("去年上半年"));
    }

    /// 🔴 四位数字**不跟 `年`** 时不许当年份：`1032` 是公司编码、`2024` 可能是商品型号。
    /// 判宽了会把实体编码解析成时间窗，那是静默答错。
    #[test]
    fn four_digits_without_year_char_is_not_a_year() {
        assert_eq!(explicit_year("1032编码本月"), None);
        assert_eq!(explicit_year("2024型号"), None);
        assert_eq!(explicit_year("2025年的数"), Some(2025));
        // 位数不对：三位/五位都不算
        assert_eq!(explicit_year("202年"), None);
        assert_eq!(explicit_year("20250年"), None);
        // 范围外
        assert_eq!(explicit_year("1999年的数"), None);
        // 「近3年」不是显式年份
        assert_eq!(explicit_year("近3年的数"), None);
    }

    /// 「top」前随字母不算 TopN（stop3/desktop5）；最高级分支认第二次出现
    #[test]
    fn top_n_word_boundary_and_second_occurrence() {
        assert_eq!(detect_top_n("desktop5 的销量"), 200, "desktop 里的 top 不是 TopN");
        assert_eq!(detect_top_n("top5 客户"), 5);
        // 「最高…最好5个…」：第一次出现没带数字时，第二次出现仍要命中
        assert_eq!(detect_top_n("销量最高和最好的5个商品"), 5);
        // 「最近销量，近7天呢」：「最近」后无数字时，句尾「近7天」仍要命中
        assert!(recent_n("最近销量，近7天呢").is_some());
        assert!(time_predicate("最近销量，近7天呢").unwrap().contains("INTERVAL 6 DAY"));
    }

    /// 单字「近」不再让「附近/接近」误判当期（RequireTimeCap 误造闸的回归钉）
    #[test]
    fn nearby_is_not_a_time_window() {
        assert!(!window_includes_today("附近的门店销量"));
        assert!(!window_includes_today("接近目标的门店"));
        assert!(window_includes_today("近7天的销量"));
        assert!(window_includes_today("最近三个月销量"));
    }

    /// 🔴 无显式年份那一支**字节不变**（既有断言与 prompt golden 都钉着它）。
    #[test]
    fn this_year_branch_keeps_its_bytes() {
        assert_eq!(
            tp("上半年"),
            "{} >= DATE_FORMAT(CURDATE(),'%Y-01-01') AND {} < DATE_FORMAT(CURDATE(),'%Y-07-01')"
        );
        assert!(tp("第二季度").contains("CONCAT(YEAR(CURDATE()),'-04-01')"));
        assert!(tp("6月的数").contains("CONCAT(YEAR(CURDATE()),'-06-01')"));
    }

    #[test]
    fn time_col_is_parameterized() {
        // 谓词模板列名可填——同一解析结果给不同表用不同时间列
        let tpl = time_predicate("本月").unwrap();
        assert!(fill_time_col(&tpl, "after_sales_time").contains("after_sales_time"));
        assert!(!fill_time_col(&tpl, "after_sales_time").contains("{}"));
    }

    #[test]
    fn explicit_iso_date_range_is_inclusive_and_validated() {
        assert_eq!(
            tp("湖南省 2026-08-03 至 2026-08-09 销售额"),
            "{} >= '2026-08-03' AND {} < DATE_ADD('2026-08-09', INTERVAL 1 DAY)"
        );
        assert_eq!(
            tp("查询范围：2024-02-29 到 2024-03-01"),
            "{} >= '2024-02-29' AND {} < DATE_ADD('2024-03-01', INTERVAL 1 DAY)"
        );
        assert!(time_predicate("2026-13-03 至 2026-14-09").is_none());
        assert!(time_predicate("2026-02-30 至 2026-03-01").is_none());
        assert!(time_predicate("2025-02-29 至 2025-03-01").is_none());
        assert!(time_predicate("2026-08-09 至 2026-08-03").is_none());
        assert!(time_predicate("型号 2026-08-03").is_none(), "单个日期不能被误当范围");
        assert_eq!(
            strip_explicit_date_range("湖南省 2026-08-03 至 2026-08-09 销售额"),
            Some("湖南省  销售额".into())
        );
        assert!(strip_explicit_date_range("至臻商品 2026-08-03").is_none());
    }

    #[test]
    fn cn_num_parses() {
        assert_eq!(cn_num("3"), Some(3));
        assert_eq!(cn_num("三"), Some(3));
        assert_eq!(cn_num("十"), Some(10));
        assert_eq!(cn_num("十二"), Some(12));
        assert_eq!(cn_num("三十"), Some(30));
        assert_eq!(cn_num("三十五"), Some(35));
        assert_eq!(cn_num("两"), Some(2));
        assert_eq!(cn_num("abc"), None);
    }

    /// 五条规则的先后顺序即行为：调换任意两条都会让这组断言变红。
    #[test]
    fn rule_order_is_behavior() {
        // 「最近三个月」：规则①先跑，不得被规则④当成「3 月」
        assert!(tp("最近三个月").contains("INTERVAL 3 MONTH"));
        assert!(!tp("最近三个月").contains("-03-01"));
        // 「上个月」：规则④主动排除「个月」，交规则⑤兜底
        assert!(tp("上个月").contains("INTERVAL 1 MONTH"));
        // 「6月」：规则④命中，不落到规则⑤
        assert!(tp("6月").contains("-06-01"));
        assert!(tp("十二月").contains("-12-01"));
        // 「第二季度」：规则②先于规则④（否则「二」旁的「月」字无从解析，此处仅锁季度命中）
        assert!(tp("第二季度").contains("-04-01"));
        assert!(tp("上半年").contains("-01-01"));
    }

    /// 每条规则都产模板而非成品谓词：漏写 `{}` 会让调用方拼出裸列名 SQL。
    #[test]
    fn every_rule_yields_template() {
        for q in ["近7天", "第二季度", "本季度", "上季度", "上半年", "下半年", "6月",
                  "今天", "昨天", "前天", "本月", "上月", "本周", "上周", "今年", "去年"] {
            let tpl = tp(q);
            assert!(tpl.contains("{}"), "{q} → {tpl}");
            assert!(!fill_time_col(&tpl, "c").contains("{}"), "{q}");
        }
        assert!(time_predicate("没有时间词").is_none(), "无时间词不得臆造时间窗");
    }

    /// 上期窗口同样是模板（原先写死列名）；标签与窗口一一对应。
    #[test]
    fn prev_window_is_template() {
        for q in ["今天", "昨天", "本月", "上月", "本周", "今年"] {
            let (tpl, label) = prev_window(q).unwrap_or_else(|| panic!("未解析: {q}"));
            assert!(tpl.contains("{}"), "{q} → {tpl}");
            assert!(label.starts_with('较'), "{q} → {label}");
        }
        assert!(prev_window("上半年").is_none());
    }

    #[test]
    fn yoy_window_uses_same_progress_periods() {
        for q in ["今天销售额", "昨天销售额", "本月销售额", "上月销售额", "本周销售额"] {
            let (tpl, label) = yoy_window(q).unwrap_or_else(|| panic!("未解析同比: {q}"));
            assert!(tpl.contains("{}"), "{q} → {tpl}");
            assert_eq!(label, "同比");
        }
        let month = yoy_window("本月销售额").unwrap().0;
        assert!(month.contains("CURDATE() - INTERVAL 1 YEAR"), "{month}");
        assert!(!month.contains("DATE_FORMAT(CURDATE(),'%Y-%m-01')"), "不能拿去年整月比本月至今: {month}");
        let week = yoy_window("本周销售额").unwrap().0;
        assert!(week.contains("INTERVAL 364 DAY"), "周同比必须保持星期结构: {week}");
        assert!(yoy_window("近三个月销售额").is_none());
    }

    /// 🔴「当月」与「本月/这个月」必须是同一档：此前只有 `rule_relative` 认它，
    /// 于是「当月销售额」算得出窗口却拿不到「较上月」和同比两个角标（2026-08-13 审计）。
    #[test]
    fn current_month_synonyms_share_one_caliber() {
        for q in ["本月销售额", "这个月销售额", "当月销售额"] {
            assert_eq!(
                time_predicate(q),
                time_predicate("本月销售额"),
                "{q} 的时间窗与「本月」不同"
            );
            assert_eq!(prev_window(q), prev_window("本月销售额"), "{q} 缺环比");
            assert_eq!(yoy_window(q), yoy_window("本月销售额"), "{q} 缺同比");
        }
    }

    /// 「近一周」与「近7天」必须是同一个窗口。
    ///
    /// 🔴 由来（2026-08-15 并行猎捕实测）：上界含今天，而周/月/年的下界只回推整 N 个单位，
    /// 于是「近一周」覆盖 8 个自然日、「近7天」覆盖 7 个 —— 同一句话两种说法两个数。
    #[test]
    fn a_rolling_window_is_the_same_length_however_you_say_it() {
        let week = tp("近一周销售额");
        assert!(week.contains("INTERVAL 1 WEEK"), "{week}");
        assert!(week.contains("INTERVAL 1 DAY)"), "周窗必须补回一天才是 7 天：{week}");
        // 天那一支照旧回推 N-1（不是 +1 天的写法），别被这条改动带偏
        assert!(tp("近7天销售额").contains("INTERVAL 6 DAY"), "{}", tp("近7天销售额"));
        for q in ["近三个月销售额", "近2年销售额"] {
            assert!(tp(q).contains("INTERVAL 1 DAY)"), "{q} 的下界也要补回一天：{}", tp(q));
        }
    }

    #[test]
    fn recent_n_units_and_bounds() {
        assert_eq!(recent_n("近7天"), Some((7, "DAY")));
        assert_eq!(recent_n("过去两周"), Some((2, "WEEK")));
        assert_eq!(recent_n("最近三个月"), Some((3, "MONTH")));
        assert_eq!(recent_n("近十五天"), Some((15, "DAY")));
        assert_eq!(recent_n("近2年"), Some((2, "YEAR")));
        assert_eq!(recent_n("近99天"), None, "N>60 不解析");
        assert_eq!(recent_n("最近一段时间"), None, "无单位不解析");
    }

    /// 🔴 「近 N 天」含今天 = 恰好 N 个自然日：起点只回推 N-1 天。
    /// 修前回推 N 天 → 窗口 N+1 天（「近7天」实测覆盖 8 个自然日），
    /// 与「含今天 7 天」的业务口径不符（CODE-REVIEW-2026-07-30 第 2 条）。
    #[test]
    fn recent_n_days_window_is_exactly_n_days_including_today() {
        let p = tp("近7天销售额");
        assert!(p.contains("INTERVAL 6 DAY"), "近7天=今天+前6天：{p}");
        assert!(!p.contains("INTERVAL 7 DAY"), "回推 7 天就是 8 个自然日：{p}");
        assert!(p.contains("< DATE_ADD(CURDATE(), INTERVAL 1 DAY)"), "右端不变（含今天）：{p}");
        assert!(tp("近1天销售额").contains("INTERVAL 0 DAY"), "近1天=只含今天");
        assert!(tp("近三十天销量").contains("INTERVAL 29 DAY"));
        // 滚动周期单位不做天数折算：周/月/年原样
        assert!(tp("最近三个月").contains("INTERVAL 3 MONTH"));
        assert!(tp("过去两周").contains("INTERVAL 2 WEEK"));
    }

    #[test]
    fn top_n_bounds() {
        assert_eq!(detect_top_n("前5"), 5);
        assert_eq!(detect_top_n("前十"), 10);
        assert_eq!(detect_top_n("前十二名"), 12);
        assert_eq!(detect_top_n("前三十五名"), 35);
        assert_eq!(detect_top_n("top20"), 20);
        assert_eq!(detect_top_n("TOP3"), 3);
        // 未提 TopN → 200（50 会把 60 个分组静默截成 50）
        assert_eq!(detect_top_n("按月分组"), 200);
        assert_eq!(detect_top_n("前999"), 200, "越界不采纳");
    }

    /// 🔴 「最高的 N 个」＝「前 N」。
    ///
    /// 它存在的理由是**解锁一道题进确定性路径的前提**：
    /// 「…销量最高的5个商品分类是哪些」的 gold 只要 5 行，若 TopN 认不出就按 200 行出数
    /// → 行数不符、**确定性地失败**。只放宽残留守卫而不补这一支，
    /// 等于把「飘着的失败」换成「确定的失败」。
    #[test]
    /// 「倒数 N 名」＝「最低的 N 个」：方向 ASC、条数 N。
    ///
    /// 🔴 由来（2026-08-15 生产直打）：ASC+LIMIT N 的能力早就有，只是「倒数」这个说法
    /// 在四处词表里一处都没有 —— 「本月销售额倒数三名的省区」白拒。
    #[test]
    fn a_bottom_n_phrase_is_a_reverse_top_n() {
        assert_eq!(detect_top_n("本月销售额倒数三名的省区"), 3);
        assert_eq!(detect_top_n("倒数5个商品"), 5);
        // 不带数字的「倒数」不产生截断（与其它极值词同规矩）
        assert_eq!(detect_top_n("销售额倒数的客户"), super::MAX_TOP_N.max(200));
    }

    fn detect_top_n_superlative_form() {
        assert_eq!(detect_top_n("销量最高的5个分类"), 5);
        assert_eq!(detect_top_n("费用最多的前5个项目"), 5, "「前」那支先命中，结果相同");
        assert_eq!(detect_top_n("金额最高的10名客户"), 10);
        assert_eq!(detect_top_n("销量最高的三个分类"), 3, "中文数字");
        assert_eq!(detect_top_n("销量最高的十二个分类"), 12);
        assert_eq!(detect_top_n("销量最高的三十五个分类"), 35);
        assert_eq!(detect_top_n("库存最少的2项"), 2);
        assert_eq!(detect_top_n("本月卖得最好的10个商品"), 10);
        // 「最低/最差」与同族六词同等待遇（少了它们，这两句的 TopN 认不出来 → 按 200 行出数）
        assert_eq!(detect_top_n("销售额最低的5个客户"), 5);
        assert_eq!(detect_top_n("卖得最差的3个商品"), 3);
        // 🔴 判据刻意窄：**不认光秃秃的「N个」** —— 那可能是值过滤里的数量词，
        // 按它截断就是悄悄改语义
        assert_eq!(detect_top_n("5个仓库的库存金额"), 200, "不许把值过滤当 TopN");
        assert_eq!(detect_top_n("最高的销量是多少"), 200, "没有数量词 → 不是 TopN");
        // 越界不采纳
        assert_eq!(detect_top_n("销量最高的999个分类"), 200);
        assert_eq!(detect_top_n("本月最好的商品是哪个"), 200, "没有数量词 → 不是 TopN");
    }

    /// 🔴「前三季度」= Q1+Q2+Q3（年初→9 月底），**不是**第三季度。
    ///
    /// 修前两处各自独立地错：
    /// 1. `rule_quarter` 取「季度」前 3 个字当 head（「年前三」），只在里头找序数「三」
    ///    → start_month=7 → 出 **7-9 月单季**，head 里的「前」被当空气；
    /// 2. `detect_top_n` 的「前」分支不看后面跟的是什么单位 → 同一句还被判成 TopN=3，
    ///    在错窗口之上再叠一层「只取 3 行」。
    ///
    /// 而这个窗口是当作「已按规则解析、直接照用」写进 prompt 的（`agent/gather.rs:113`
    /// 把区间交给 LLM），等于把错口径当权威 —— 与本文件 `explicit_year` 那条缺陷同源。
    #[test]
    fn leading_n_quarters_is_year_start_through_that_quarter_not_that_single_quarter() {
        let p = tp("今年前三季度的金额");
        assert!(p.contains("-01-01"), "前三季度必须从年初起：{p}");
        assert!(p.contains("-10-01"), "右端必须是 10-01（＝到 9 月底的半开区间）：{p}");
        assert!(!p.contains("-07-01"), "不许出成 Q3 单季：{p}");
        // 显式年份/去年那一支（字面日期）同样成立，含跨年右端
        assert_eq!(tp("2025年前三季度金额"), "{} >= '2025-01-01' AND {} < '2025-10-01'");
        assert_eq!(tp("2024年前四季度"), "{} >= '2024-01-01' AND {} < '2025-01-01'");
        assert!(tp("去年前三季度").contains("INTERVAL 1 YEAR"), "{}", tp("去年前三季度"));
        // 「前三个季度」＝「前三季度」（末尾的「个」不改语义）
        assert_eq!(tp("前三个季度的销量"), tp("前三季度的销量"));

        // 🔴 反面①：真序数一个都不许被改口径（把上面那支写宽一点这几条就红）
        assert!(tp("第三季度").contains("CONCAT(YEAR(CURDATE()),'-07-01')"), "{}", tp("第三季度"));
        assert!(!tp("第三季度").contains("-01-01"), "{}", tp("第三季度"));
        assert_eq!(tp("2024年第三季度"), "{} >= '2024-07-01' AND {} < '2024-10-01'");
        assert!(tp("三季度金额").contains("-07-01"), "{}", tp("三季度金额"));
        assert!(tp("本季度").contains("QUARTER(CURDATE())"), "{}", tp("本季度"));
        assert!(tp("上季度").contains("INTERVAL 3 MONTH"), "{}", tp("上季度"));

        // 🔴 反面②：「近/最近/过去 N 季度」是滚动窗（起点随今天漂）—— 修前出的是第 N 季度，
        // 现在必须一个窗口都不出（错窗口比没窗口更坏）
        assert!(rule_quarter("最近三个季度的销量").is_none());
        assert!(rule_quarter("过去三个季度的销量").is_none());
        assert!(time_predicate("过去三个季度的销量").is_none(), "整条链上也不许兜出别的窗口");
    }

    /// 🔴「前三季度」这类时间窗不许被当成 TopN（修前 = 3，静默把结果截成 3 行）。
    #[test]
    fn top_n_ignores_time_windows() {
        assert_eq!(detect_top_n("今年前三季度的金额"), 200, "时间窗不是 TopN");
        assert_eq!(detect_top_n("前30天的金额"), 200);
        assert_eq!(detect_top_n("前两个月的销量"), 200);
        assert_eq!(detect_top_n("前3天的订单数"), 200);
        // 🔴 反面：真 TopN 一条都不许丢（这也是为什么不用「数量单位白名单」——
        // 「前十」「前十的客户」后面没有量词，白名单会把它们判成 200）
        assert_eq!(detect_top_n("前3个客户"), 3);
        assert_eq!(detect_top_n("前十"), 10);
        assert_eq!(detect_top_n("前三名商品分类"), 3);
        assert_eq!(detect_top_n("金额前十的客户"), 10);
        assert_eq!(detect_top_n("前5"), 5);
        // 时间窗后面还跟着真 TopN 时取后者（一句里「前」不止一个）
        assert_eq!(detect_top_n("今年前三季度金额前5的客户"), 5);
        assert_eq!(detect_top_n("今年前三季度卖得最好的10个商品"), 10,
                   "时间窗里的「前」应忽略，后续真正的排行仍要识别");
    }

    /// 🔴 环比必须比**同期**。修前「本月」的上期是**整个上月**（`>= 上月初 AND < 本月初`），
    /// 当期却只有「月初至今」的数据；「今年」那档是**去年整年** vs 年初至今。
    /// `semantic::present::patch_kpi_delta` 拿这两个数直接 `(cur-prev)/prev*100` 塞进
    /// `items[].delta`，前端照显示「较上月 -87%」—— **这是错数**。按天数即可看出与业务无关：
    /// 7 月 2 日那天当期 2 天 / 上期 30 天 ⇒ 环比恒 ≈ -93%。
    ///
    /// 判据形态刻意不是「上期含某个字符串」，而是**由当期推出上期**：
    /// 右端必须是「今天」整体平移一期，左端必须是当期左端把锚点平移一期。
    #[test]
    fn prev_window_shares_the_today_anchor_with_the_current_window() {
        for (q, shift) in [
            ("本月金额", " - INTERVAL 1 MONTH"),
            ("本周金额", " - INTERVAL 7 DAY"),
            ("今年金额", " - INTERVAL 1 YEAR"),
        ] {
            let (tpl, _) = prev_window(q).unwrap_or_else(|| panic!("未解析: {q}"));
            let right = tpl
                .rsplit_once("AND {} < ")
                .unwrap_or_else(|| panic!("上期没有「今天」锚点的右端: {q} → {tpl}"))
                .1;
            assert_eq!(
                right.strip_suffix(shift),
                Some("CURDATE()"),
                "{q} 的上期右端不是「今天{shift}」：{right}"
            );
        }
        // 左端：当期左端把锚点平移一期，逐字相同（只挪锚点，不换形状）
        let cur_left = tp("本月").split(" AND ").next().unwrap().to_string();
        let prev_left = prev_window("本月").unwrap().0.split(" AND ").next().unwrap().to_string();
        assert_eq!(prev_left, cur_left.replace("CURDATE()", "CURDATE() - INTERVAL 1 MONTH"));
        // 今年：当期那支是 `YEAR({}) = YEAR(CURDATE())`（没有左端可推），左端必须是去年年初
        let year_prev = prev_window("今年").unwrap().0;
        assert!(
            year_prev.starts_with("{} >= DATE_FORMAT(CURDATE() - INTERVAL 1 YEAR,'%Y-01-01')"),
            "{year_prev}"
        );

        // 🔴 反面：单日与「当期本身就完整」的那几档本来没这个问题，一个字都不许动
        assert_eq!(prev_window("今天"), Some(("DATE({}) = CURDATE() - INTERVAL 1 DAY", "较昨天")));
        assert_eq!(prev_window("昨天"), Some(("DATE({}) = CURDATE() - INTERVAL 2 DAY", "较前天")));
        assert_eq!(
            prev_window("上月"),
            Some((
                "{} >= DATE_FORMAT(CURDATE() - INTERVAL 2 MONTH,'%Y-%m-01') AND {} < DATE_FORMAT(CURDATE() - INTERVAL 1 MONTH,'%Y-%m-01')",
                "较上上月"
            )),
            "「上月」当期是完整期，上期就该是整个上上月"
        );
    }

    /// `time_cap='yesterday'` 的窗口右端：追加 `< CURDATE()`，左端不动；
    /// 已排除今天的窗口追加后语义不变形（冗余条件，比按模板形态分支处理稳）。
    #[test]
    fn cap_at_yesterday_appends_upper_bound() {
        let tpl = time_predicate("本月").unwrap();
        let capped = cap_at_yesterday(&tpl);
        assert!(capped.starts_with(&tpl), "左端不许动：{capped}");
        assert!(capped.ends_with("AND {} < CURDATE()"), "{capped}");
        // 「今天」追加后是空集 —— 对发货类指标这正是事实（今天的单还没发货）
        let today = cap_at_yesterday(&time_predicate("今天").unwrap());
        assert!(today.contains("DATE({}) = CURDATE() AND {} < CURDATE()"), "{today}");
        // 填入列名后是合法谓词形态
        let filled = fill_time_col(&capped, "delivery_time");
        assert!(filled.contains("delivery_time < CURDATE()"), "{filled}");
    }

    /// `window_includes_today` 词表与 `rule_relative` 的当期分支一一对应：
    /// 当期判据（RequireTimeCap）只在窗口含今天时才造，往期间法判了就是误伤。
    #[test]
    fn window_includes_today_wordlist() {
        for q in ["今天", "今日", "本月", "这个月", "当月", "本周", "这周",
                  "今年", "本年", "年初至今", "近三个月", "近7天", "最近的"] {
            assert!(window_includes_today(q), "{q} 是当期");
        }
        for q in ["昨天", "上月", "上个月", "去年", "上周", "2025年上半年", "第四季度", "6月"] {
            assert!(!window_includes_today(q), "{q} 是往期");
        }
    }

    /// 【A17 ①】时间表面词提取：最长最具体优先；显式年份一律 None（继承会改年份）。
    #[test]
    fn time_phrase_of_picks_first_specific() {
        assert_eq!(time_phrase_of("上个月总量是多少"), Some("上个月"));
        assert_eq!(time_phrase_of("本月前10个商品"), Some("本月"));
        assert_eq!(time_phrase_of("今年7月走势"), Some("今年"));
        assert_eq!(time_phrase_of("2025年上半年走势"), None, "显式年份不继承");
        assert_eq!(time_phrase_of("品类第二的呢"), None);
        assert_eq!(time_phrase_of("近三个月各品类走势"), Some("近三个月"));
    }
}
