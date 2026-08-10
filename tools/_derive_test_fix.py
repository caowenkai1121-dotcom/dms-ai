# -*- coding: utf-8 -*-
"""更新 derive 接线测试：适配两轮壳 + derive_attempt 拆分。"""
import io

p = 'crates/server/src/direct.rs'
s = io.open(p, encoding='utf-8', newline='').read()

old = """        // ③④ ods_derive 本体：候选校验 → 闸门 → 预执行，顺序即行为
        let body = src
            .split("async fn ods_derive(")
            .nth(1)
            .expect("ods_derive 没了")
            .split("\\nfn customer_name_fragment(")
            .next()
            .expect("ods_derive 函数边界没了");
        let allow = body.find("derive_tables_allowed").expect("用表硬校验没了");
        let gate = body.find("dms_agent::gate_on").expect("推导必须过与直连同一个 gate_on");
        assert!(allow < gate, "用表硬校验必须在闸门之前：{body}");
        assert!(body.contains("dms_agent::MAX_ROWS") && body.contains("dms_agent::EXEC_TIMEOUT"),
                "行上限/超时不许另搞一套：{body}");
        assert!(body.contains("route: DERIVE_ROUTE.into()"), "命中必须带 direct-derive route：{body}");
        // 预执行（fetch）必须在 Some(DirectHit) 之前 —— 执行失败要回落原卡
        let fetch = body.find("cx.source.fetch").expect("预执行没了");
        let hit = body.find("Some(DirectHit").expect("命中构造没了");
        assert!(fetch < hit, "必须先预执行成功才许产出推导命中：{body}");"""

new = """        // ③④ 推导本体（ods_derive 两轮壳 + derive_attempt 单轮体）：
        //    候选校验 → 闸门 → 预执行在 derive_attempt 里，顺序即行为
        let body = src
            .split("async fn derive_attempt(")
            .nth(1)
            .expect("derive_attempt 没了")
            .split("\\nfn customer_name_fragment(")
            .next()
            .expect("derive_attempt 函数边界没了");
        let allow = body.find("derive_tables_allowed").expect("用表硬校验没了");
        let gate = body.find("dms_agent::gate_on").expect("推导必须过与直连同一个 gate_on");
        assert!(allow < gate, "用表硬校验必须在闸门之前：{body}");
        assert!(body.contains("dms_agent::MAX_ROWS") && body.contains("dms_agent::EXEC_TIMEOUT"),
                "行上限/超时不许另搞一套：{body}");
        // 预执行（fetch）必须在 DeriveTry::Hit 之前 —— 执行失败/零行都不许产出命中
        let fetch = body.find("cx.source.fetch").expect("预执行没了");
        let hit = body.find("DeriveTry::Hit(candidate)").expect("命中构造没了");
        assert!(fetch < hit, "必须先预执行成功才许产出推导命中：{body}");
        let shell = src
            .split("async fn ods_derive(")
            .nth(1)
            .expect("ods_derive 没了")
            .split("async fn derive_attempt(")
            .next()
            .expect("ods_derive 函数边界没了");
        assert!(shell.contains("route: DERIVE_ROUTE.into()"), "命中必须带 direct-derive route：{shell}");
        assert!(shell.contains("DeriveTry::Empty") && shell.contains("tried.extend"),
                "空结果必须剔除试过的表再来一轮：{shell}");"""

assert s.count(old) == 1, s.count(old)
s = s.replace(old, new)
io.open(p, 'w', encoding='utf-8', newline='').write(s)
print('test updated')
