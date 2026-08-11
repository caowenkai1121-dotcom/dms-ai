# 优化清单（六角色评审 + 全仓分文件审计 + 三路调研）

共 3348 条（safe 2228 / test 1120）。来源：全仓逐文件审计 swarm×2 + DMS 后端源码校准 + 开源系统差距 + 小程序集成点。


## chat.vue（71 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| chat.vue:37 | chat-scroll 未开 `enhanced`/`show-scrollbar="false"`，长聊天记录滚动性能与观感有小改进空间 | 加 `:enhanced="true" :show-scrollbar="false"`（小程序端生效） | safe |
| chat.vue:31-34,6141-6152 | 「切换」入口高仅 48rpx（≈24px），低于可点区域推荐值，店里手湿/戴手套难点中 | 加 min-height 或上下 padding 撑到 ≥64rpx | safe |
| chat.vue:217 | 门店名用 `shopInfo && shopInfo.shopName` 无兜底，与 219-223 行统一 ` |  | '-'` 写法不一 |
| chat.vue:248-251 | 「▶️ 直接开始」按钮不含类型名，追问气泡被新消息顶掉后用户看不到上次巡的是哪块 | 按钮文案带类型，如「直接开始（冻品+鲜食）」 | safe |
| chat.vue:276 | 「识别中，请稍候...」半角三点，与 299 行「解析中…」、459 行「处理中，请稍候...」全/半角省略号混用 | 统一为「…」 | safe |
| chat.vue:292-295 | 手写框 maxlength=800 静默截断，粘贴长文被截用户无感知 | 达到上限时 toast 提示一次「已到 800 字上限」 | safe |
| chat.vue:343 | 上传徽标「传1/3」过简，新用户看不懂 | 改「上传 1/3」 | safe |
| chat.vue:421,1451 | 低置信提示直接展示 `toFixed(2)` 原始分（如 0.42），对巡店员是黑话 | 改自然语言（「把握不大」）或至少加「置信度」前缀 | safe |
| chat.vue:663 | phase 注释枚举缺 customer/noCustomer/noStore/reportChoice 四个实际存在的阶段 | 补全注释 | safe |
| chat.vue:906-927 | 看门狗 65s 触发，toast 却说「别超过1分钟」，数字对不上易困惑 | toast 改「说话时间太长已自动停止，请分段说」 | safe |
| chat.vue:978 | 提示「可在右上角开启【自动确认】」——开关早已挪到 dock 上方一行（164-174 行注释明确写了避让胶囊挪出），文案指向错误位置 | 改「可在上方开启【自动确认】」 | safe |
| chat.vue:1081-1085 | 合并段注释写「仅实时长段」，但 manual 长段同样参与合并（按 source 分组） | 注释补「手动输入长段同规则合并」 | safe |
| chat.vue:1131 | runWhenVoiceIdle 忙时每点一次都 toast「语音识别中，稍等…」，连点刷屏 | toast 节流（1s 内只弹一次） | safe |
| chat.vue:1175 | 离线横幅「请手动输入」但采集语音态需先切「文字」模式，文案没指路 | 补「可点下方『文字』切换输入」 | safe |
| chat.vue:1204 | `statusCode === 0` 直接判网络错误，无注释说明依据 | 加注释说明 0 在哪些端代表网络层失败 | safe |
| chat.vue:1450,2853,4343,4374,4398 | 0.6（黄线）/0.5（红线）/0.75（人工清除）置信阈值多处硬编码，调阈值要改一圈 | 提常量 CONF_YELLOW/CONF_RED/CONF_CLEARED | safe |
| chat.vue:1471,2519 | 「红线」category 关键字字面量两处手写，后端改名要同时改 | 提共享常量 REDLINE_CAT_KEY | safe |
| chat.vue:1689 | 所有 ai 消息统一 350-600ms 假打字延迟，红线警示（2486）/失败重试等紧急反馈也被无意义拖慢 | warn/retry 角色跳过 typing 延迟直出 | test |
| chat.vue:1668-1672,1727-1731 | bumpModuleCard 复用同 id 卡时 scrollAnchor 值不变，`scroll-into-view` 同值赋值不重触发，卡可能不滚到底 | 先置 '' 再 nextTick 赋目标值 | test |
| chat.vue:1753 | parentNumeric 正则不排负号，「-5」会取到 5 误判「父指标值大于0」，与注释「首个非负数」不符 | 匹配前排除负号（如 `(?<!-)\d` 或先判首字符） | test |
| chat.vue:1791 | isRequiredNow 仅是 isHardRequired 的无参数别名转发，无附加逻辑 | 内联替换或注释保留理由（语义分层） | safe |
| chat.vue:650-660,1807-1810 | getUserInfo 每次调用都同步读 storage，autoFillBaseValue 按指标循环时重复读 | 页面级缓存一次 userInfo | safe |
| chat.vue:1835 | 门店匹配唯一命中的置信判断借用实体阈值常量 THRESH_PICK，语义不同源 | 单定义 SHOP_AUTO_PICK_MIN 或注释借用原因 | safe |
| chat.vue:1879 | `已选择：${shopName}店` 机械拼「店」，店名本身以「店」结尾时变「XX店店」 | 去后缀或改「已选择门店：X」 | safe |
| chat.vue:1899-1908 | confirmStoreYes 内 ensureLocationHigh().then 回传坐标无 sessionGen 守卫，快速切店后可能把新店定位回传到旧店 shopCode | 闭包前快照 gen，then 内比对 | test |
| chat.vue:2058-2102 | showLastIssues 无会话代际守卫（loadMasterValues 2035-2038 有），A 店 getLastIssues 迟到响应会写进 B 店的 lastIssueMap/lastValuesMap/lastGrade | 入口快照 gen，响应落地前比对 | test |
| chat.vue:2181 | 生产代码残留 console.log（空列表回退埋点） | 删除或统一埋点通道 | safe |
| chat.vue:2189-2200 | 另一业务字典预取 .then 无代际守卫，带 A 店 scope 的迟到响应会写 loadedIndicators[bt] 并 mergeRuleMeta，污染 B 店字典缓存与规则 | then 内先判 `gen !== sessionGen` 丢弃 | test |
| chat.vue:2229-2230 | maybeShowFirstGuide 在 presentGroup（bumpModuleCard 把卡顶到末尾）之后再 pushMessage，教学气泡落在模块卡下方，违背 1722 行「卡总在聊天流最下方」的设计注释 | 先推教学气泡再 presentGroup | safe |
| chat.vue:2439-2440 | 注释「本模块 source=auto（沿用上次预填）项」未提台账 master 行也走此段（2534 isMasterRow 区分标签） | 注释补「含台账带出」 | safe |
| chat.vue:2523-2542 | isCarryOver/isMasterRow 每行每次渲染都 JSON.parse(valueJson)，summaryVm(5951)/checklist(2454) 多处循环调用 | detail 落值时预解析挂标志位，或 WeakMap memo | safe |
| chat.vue:2651 | applyStoreRelative 落同句明确值时 buildPlainDetail 未传 item.valueJson 与 detailSource（主路径 3489 两者都传），主推品价格结构化 items 在此路径丢失 | 对齐主路径参数列表 | test |
| chat.vue:2901-2904 | `if (false && ...)` 死分支 + wholeCheckHinted 成只写变量（685/696/2902），注释虽说明保留但已是纯噪音 | 删除死代码与变量，或抽 NAMED 开关常量 | safe |
| chat.vue:2971 | toast「最多撤回最近5步」用「撤回」，其余文案（2805/2978/2994）统一用「撤销」 | 统一为「撤销」 | safe |
| chat.vue:2994 | `(恢复为 "…")` 用英文直引号，与全站「」【】风格不一 | 改中文引号 | safe |
| chat.vue:3108,3154,3337,3353 | 同一条去标点正则复制 4 份，改一处漏三处 | 提常量 CLEAN_RE / cleanSpoken() 工具 | safe |
| chat.vue:3155 | 业务态肯定词内联数组（含'对的''嗯''行''可以'）与 610 行 CONFIRM_WORDS 不一致，同一句「对的」在汇总态(3111)不识别、业务态识别 | 统一词表或注释差异原因 | test |
| chat.vue:3299-3315,6106-6112 | tokenExpiredPrompted 置位后页面生命周期内永不复位，重新登录后再次失效不再弹登录提示（sessionExpired 在 onShow 复位了，提示位没有） | onShow 补提成功路径一并复位 tokenExpiredPrompted | test |
| chat.vue:3472 | pendingModify 的 oldValue 取 summaryValueText(ind) 读的是 moduleDraft，与 details 里「已确认值」可能不同，追问「从 X 改成 Y」的 X 应是确认层值 | oldValue 改读 details 层 | test |
| chat.vue:3627 | 重试文案「（或点「本模块说完了」手动补）」括号套「」可读性差，且「识别服务开小差了」与 3394 重复维护 | 文案提常量/改写句式 | safe |
| chat.vue:3644,3789,3958 | multi_enum 兜底切分正则 `[、,，\s]+` 含 `\s`，带空格的商品名（「爆汁 肠」）被切碎成两项 | 字符集去 `\s`，切完各段 trim | test |
| chat.vue:3723 | handleEditVoice 任何异常（业务错/解析失败）都 toast「识别超时」，误导 | 改「识别失败，手动填一下，或重说」 | safe |
| chat.vue:3927 | resolveMultiEntity 每个未命中词各弹一次 toast，多词全不中时后面的覆盖前面的，用户只看到最后一条 | 聚合成一条「X、Y 没匹配到，手动改一下」 | safe |
| chat.vue:4185 | 注释内用弯引号“其他指标附件”，全文件其余注释统一「」 | 统一为「」 | safe |
| chat.vue:4398 | clearSuspicion 把 confidence 直接改 0.75，魔法数无出处（高于 0.6 黄线、低于 0.8 预填档） | 提常量并注释取值依据 | safe |
| chat.vue:4560 | sameSep 只归一 `,，、`，aiValue 若含全角分号/空格分隔仍被误判人工改写，误清 aiValueJson 并上报假纠错样本 | 扩展分隔符集或注释限定范围 | test |
| chat.vue:4651-4653 | `!g |  | !moduleReady.value` 合并 toast「还有必填项没填齐」，g 为 null（指针越界）时也这么说，误导 |
| chat.vue:4719-4720,4728 | 手动点「跳过」走 confirmModule(true)，随后回声显示「已自动确认」，人工操作被说成自动 | skip 路径传独立 echo 文案（如「已跳过」） | safe |
| chat.vue:4892-4893 vs 754-755 | customerCode 取值优先级两处相反：indicatorScope 先 shopInfo 后 selectedCustomer，buildRecord 先 selectedCustomer 后 shopInfo，记录与指标 scope 可能挂不同客户 | 统一优先级 | test |
| chat.vue:4966,5364 | finishCollection 在「提交中...」loading 下 await ensureLocationHigh 最长 8s，用户以为卡死 | 高精度取位与 saveBatch 并行，或点提交前先取 | test |
| chat.vue:5045-5053 | 报告轮询只在 catch 时重试，若 aiReport 走 isTransformResponse:false 静默返回 code!==0 不抛错，会直接 break 拿空 data 渲染空报告卡 | 循环内判 code!==0 继续重试 | test |
| chat.vue:5068-5069 | 注释「后台 fire-and-forget 触发生成」但函数体无任何触发调用（报告由提交触发），注释与代码不符 | 改注释为实际行为 | safe |
| chat.vue:5137 | computeDraftProgress 必填口径只用 `required === '必填'`，与 groupComplete 的 isRequiredNow（含条件触发）不一致，草稿索引进度与站内口径有偏差 | 复用 isHardRequired 或注释量化差异 | safe |
| chat.vue:5210 | `watch([moduleDraft, details, ...], scheduleDraftAutosave, {deep:true})` 每次落值深遍历两个大对象 | 回调已节流，可改 watch 键指纹（`() => Object.keys(moduleDraft).length + ...`）降遍历成本 | test |
| chat.vue:5338-5341 | getFastLocation 单例 Promise 把「拒绝授权/失败」也永久缓存，本页生命周期内不再重试快速定位 | fail 路径清空 fastLocationPromise 允许重试 | test |
| chat.vue:5439 | 待整改卡 sub `… · ${formatMonthDay(vo.lastDate)}`，lastDate 为空时尾部拖一个「· 」 | 日期段有条件拼接 | safe |
| chat.vue:5450 | 「距你${vo.distance}m」未取整，后端给浮点会显示「123.456m」 | Math.round | safe |
| chat.vue:5549-5553 | customerShopCount 对 shopCount=''（空串）`Number('')===0` 判 0 家门店，把「未知」误判成「无门店」进 noStore 分支 | 增加 `c.shopCount !== ''` 前置判断 | test |
| chat.vue:5589-5592,5633-5636,6020-6023 | customerLabel 组装（shopCount 拼「（N店）」）同一段代码复制 3 处 | 提 mapCustomerRow 工具函数 | safe |
| chat.vue:5643 | startWithPresetShop 在 shopName/shopCode 都空时仍 doMatchShop('')，得到「先说门店名称」的门店语音态提示，与预选场景不符 | 前置判空，给「门店信息缺失，请手动输入」 | safe |
| chat.vue:5649-5651 | formatDetailVOValue 优先返回「照片 N张」，带附件照片的非图片型指标（有文本值+照片）在报告明细(1577/1585)/带走行(2448)只显示照片张数、丢掉文本值 | 非 image 且有 valueText/valueJson 时先显值再附「照片N张」 | test |
| chat.vue:5701 | `matched.inspctionStatus` 是历史拼写错误的兼容字段，无注释，后人易当 typo 顺手改掉导致旧记录状态误判 | 加注释「兼容后端旧拼写，勿改」 | safe |
| chat.vue:5710 | 自愈条件末尾 `&& id != null` 恒真（loadViewMode 入口已用 id 设 recordId），冗余 | 删除冗余条件 | safe |
| chat.vue:5744 | `await summarizeDraft(draft)` + try/catch，但 summarizeDraft(5733) 是无 async 无抛错的同步函数 | 去 await 直接调用 | safe |
| chat.vue:5901,5908 | 注释「前2个问题名」但代码 names 全量 join（靠 500 字截断兜底），注释与代码不符 | 注释改「全部问题名，播报截断兜底」或 slice(0,2) | safe |
| chat.vue:5935-5936 | moduleMissingPrompts(g) 在 promptLines 与 promptMore 各算一遍（内部对全量缺项 map buildIndicatorPrompt） | 先存局部变量复用 | safe |
| chat.vue:5987 | onSummaryEditItem 直接传 `indicatorByCode.value[code]`（可能 undefined，如跨业务过期码）给 editModuleItem，4473 行 `moduleDraft[ind.indicatorCode]` 会对 undefined 抛 TypeError；同类的 onFocusIndicator(4411) 有判空 | 比照 4411 加 `if (!ind) return` | safe |
| chat.vue:6232 | .bubble__text `word-break: break-all` 把英文/数字串（客户编码、价格）从中间折断 | 改 `overflow-wrap: anywhere` | safe |
| chat.vue:6290,6355 | suggest-cards 宽 78%、brief-card 宽 88%，同类卡片宽度不一，视觉参差 | 统一宽度或注释差异原因 | safe |
| chat.vue:6331,6516,6717,1528 | 红色至少三种（#d4380d、#e34d59、#ff4a2c/#FF4A2C）混用，大小写也不统一 | 统一色板/scss 变量 | safe |
| chat.vue:6597-6599 | 字数统计 21rpx + #aaa，对比度低、字号过小 | 字号 ≥22rpx、颜色加深一档 | safe |

## deep_api.rs（61 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| deep_api.rs:65-70,3420-3423 | `uses_dws_sales_fact` 对同一 sql 做两次 `to_ascii_lowercase()`（两次分配）；`compact_sql` 每调用必分配 | lowercase 一次存局部变量再 contains 两次 | safe |
| deep_api.rs:74-83,96 | `uses_sales_measure_contract` 每次调用都重新 `compact_sql(measure.expression()/sql_expression())`（每指标常量），且 `primary_sales_measure` 的 `find` 循环里对 `r.sql` 每个候选指标重复 compact | 表达式 compact 结果按指标缓存（OnceLock/调用方预算一次），`r.sql` 的 compact 提升到循环外 | safe |
| deep_api.rs:113-118 | BREAKDOWN_WORDS 有 "前五""前十""前10" 但缺 "前5"，"销售额前5省份" 会被误拆 | 补 "前5"（与评审批次"尾词/排行词"一族），带行为测试 | test |
| deep_api.rs:266-273 | `explicit_period_end` 用 `.nth(1)` 取第二个日期作为周期结束，无任何注释说明"为什么取第 2 个" | 加一行注释（首个=开始，次个=结束） | safe |
| deep_api.rs:275-284,1580-1585 | 两处都借 `comparison_from_values(label, 1.0, 1.0)` 仅为拿到展示 label，构造整个 Comparison 再丢弃 | 抽出 `display_label(label) -> &str` 纯函数，两处与 286-299 的分支共用 | safe |
| deep_api.rs:286-315,1770,1775,1594 | `comparison_from_values` 返回 `Option` 但函数体恒 `Some(...)`，所有 `?`/`filter_map` 都是死分支，误导读者以为会失败 | 改为直接返回 `Comparison`（内部函数，调用点同步改） | safe |
| deep_api.rs:287-288,457,1668 | baseline 为负时 `(current-baseline)/baseline` 符号翻转（如 -100→-50 显示 -50% 但 dir=up），环比/同比文案自相矛盾 | 变化率统一除以 `baseline.abs()`，带负数基期测试 | test |
| deep_api.rs:335-336,3546-3547 | impl 内多余空行、函数间双空行，与全文件紧凑风格不一致 | 删掉 | safe |
| deep_api.rs:442-443 | `yi` 在校验 `(2..=3).contains(...)` 之前计算，列数非法时白算（虽 saturating 无 panic） | 校验通过后再算 `yi` | safe |
| deep_api.rs:450-452 | "YYYY-MM" 形状校验只看长度/连字符/数字，不校验月份 01-12，"2026-99" 会被当成月度周期出"较上一期"文案 | 补 `(1..=12)` 范围校验或 `NaiveDate::parse_from_str("%Y-%m")` | test |
| deep_api.rs:468-472 | `max_by` 把 null/非数值行当 0.0 参与比大，全空板块也会产出一条"头部=0"的假 highlight | `filter_map` 先剔除非数值行，空了不出 highlight | test |
| deep_api.rs:540-544 | `filter_map` 里先算 `row_dimension_label`（字符串分配）再 `number(...)?`，值缺失时标签白分配 | 先取 number 成功再拼 label | safe |
| deep_api.rs:581-584,3326-3328 | 变化额"符号+格式化"逻辑在 evidence_items 与 bi_page 各写一份，日后改一处漏一处 | 抽 `fmt_signed_change(label, delta)` 共用 | safe |
| deep_api.rs:629 | 贡献证据里 `row.get(3)` 魔数下标取"指标"列，依赖 623 行 labels 顺序，改 labels 即静默错位 | 用命名常量或按 labels.position 查找 | safe |
| deep_api.rs:725-743 | `number_tokens` 不收录 `-`：分析写 "-20.0%" 会被截成 "20.0%"，与证据 "+20.0%"（同样截成 "20.0%"）绑定成功——符号翻转的数值主张能蒙混过闸门 | 数字 token 允许前导 `-`（仅紧挨数字时），带符号绑定测试 | test |
| deep_api.rs:732-741 | 遇 '万' 立即 flush，"1.2万亿" 只产出 token "1.2万"（=1.2e4），与证据 "12000亿"（=1.2e12）绑定失败 → 误杀整段分析 | 单位字符连续吃进（万/亿 可组合），或 flush 前 peek 下一个单位 | test |
| deep_api.rs:795-803 | `first_unbound_claim_value` 对每个 claim token 线性扫全部证据 token 并反复 `claim_value` 字符串解析 | 证据 token 预解析成 `(f64,bool)` 一次，claim 解析一次后数值比较 | safe |
| deep_api.rs:920,927 | `is_weekly_report(question)` 在同一函数内调两次 | 局部变量算一次 | safe |
| deep_api.rs:929 | `evidence_insight` 里 `llm.chat(req).await.ok()` 把 LLM 错误静默吞掉，只留降级结果，无任何日志（939 行闸门失败反而有 warn，此处更该有） | 失败时 `tracing::warn!` 一记再降级 | safe |
| deep_api.rs:981-984 | `field()` 对 body 的每个分段都 `format!("{key}=")` 分配一个新 String 只为 strip_prefix | `strip_prefix(key)` 后再 `strip_prefix('=')`，零分配 | safe |
| deep_api.rs:1072,1098 | 同函数内判断证据是否覆盖，一处用 `available(item).is_some()`（排 gap），一处用 `item.is_some()`（含 gap），无注释说明口径差异是有意的（口径板块恒为 gap，必须 is_some） | 加一行注释说明两处口径差异 | safe |
| deep_api.rs:1102-1107 | `let _follow = available(shop).or_else(...)…or_else( |  | evidence.first())?;` 链条等价于 `evidence.is_empty()` 早退，`_follow` 绑定后从不使用，纯阅读障碍 |
| deep_api.rs:1166 | `sql.split(DETAIL_SQL_SEPARATOR).next()?`：`split` 恒产出 ≥1 项，`?` 是永远不触发的死分支，暗示可能失败 | `.next().unwrap_or(sql)` 或直接索引并注释 | safe |
| deep_api.rs:1259-1261 | `compact_sql(&expr.to_string())`：先 to_string 分配一次、再 compact 分配一次 | 一次 `write!` 进 String 后原地 retain/lowercase | safe |
| deep_api.rs:1492-1499 | `let device = "";` 死变量 + format 里 `{device}` 永空占位，是早年设备过滤逻辑删除后的残留 | 删掉变量与占位符 | safe |
| deep_api.rs:1794-1796 | `raw_value` 只是 `fmt_metric` 的一行转发，无独立语义 | 内联到 1891/1901 两个调用点 | safe |
| deep_api.rs:1895,1904,1946 | `.take(14)` 魔数出现三次（单据头字段/实体字段/明细列） | 提一个 `const MAX_PRIMARY_FIELDS: usize = 14` | safe |
| deep_api.rs:2064-2069 | `progress_entry` 淘汰按**创建时间** `entry.at` 判定，写入从不刷新：繁忙实例（>200 条目）下，跑超 10 分钟的进行中报告会被中途淘汰，属主轮询突然 404 | note/note_section_state 写入时刷新 `at`（变"最后活跃"语义）；阈值 200 同时提为常量 | safe |
| deep_api.rs:2162-2163 | `done` 只看内存 steps：重启后 steps 为空，即使 PG state=done 也回 `done:false`，前端"完成即停轮询"永远等不到 | `done` 并入 `pg_row.state ∈ {done, failed}` | test |
| deep_api.rs:2268-2273 | `run_migrate` 在每次 deep_run_start/resume 都全量执行 3 条 DDL（幂等但白白 3 次往返） | 进程内 OnceCell/atomic 只跑一次（失败允许重试） | safe |
| deep_api.rs:2284-2294 | 锁中毒处理不一致：`claim_active` 用 `expect` 直接 panic，`RunGuard::drop` 却静默容忍（中毒后 rid 永锁）；同一全局锁两种策略 | 统一策略（建议都用 `unwrap_or_else( | p |
| deep_api.rs:2388-2418 | `deep_run_start` 的 INSERT run / DELETE sections / 逐条 INSERT sections 不在事务里：中途崩溃留下"running 运行 + 0 板块"的半截账本，续跑只能标 failed | 包进一个 `pool.begin()` 事务（含 2494 的 reap 两条 UPDATE 同理） | test |
| deep_api.rs:2405-2418 | 板块逐条 INSERT，N 板块 = N 次 PG 往返 | 单条多值 INSERT 或 UNNEST 批量 | test |
| deep_api.rs:2434-2448 | `deep_section_finish` 两次 UPDATE（板块终态 + 摸 run.updated_at）两次往返 | 合并为一条 CTE（`WITH s AS (UPDATE deep_section …) UPDATE deep_run …`） | test |
| deep_api.rs:2515-2567 | `deep_run_load`/`deep_sections_load` 手工大元组→结构体拆装，字段顺序错位只能靠肉眼 | `#[derive(sqlx::FromRow)]` 让字段按名绑定 | safe |
| deep_api.rs:2584 | 注释"任一时刻最多轮询两个子任务"——是**并发执行**两个，不是轮询，措辞误导 | 改为"最多并发执行两个" | safe |
| deep_api.rs:2695-2710 | 连续相同日期被去重后只剩 1 个 → 单日窗口 "2026-08-01 至 2026-08-01" 返回 None，合法单日周期识别不出 | 去重仅用于压缩重复扫描，保留"起=止"情形返回窗口 | test |
| deep_api.rs:2861-2877 | `extract_json` 的 `char_indices().skip(start)` 把 `find` 返回的**字节**下标当**字符**数跳过：'{' 前有中文等多字节字符且 JSON 嵌套时，深度配平错位、截断返回（5155 测试的扁平 JSON 只是恰好蒙对） | 改 `s[start..].char_indices()` 并用相对下标切片；补"中文前缀+嵌套 JSON"测试 | test |
| deep_api.rs:3047,3170 | `should_prefetch_plan` 为判断周报命中构造整个 `weekly_report_plan`（约 10 次 format!+Vec 分配）随即丢弃；命中后 `plan_report` 又完整重建一遍 | 预判改用 `is_weekly_report + weekly_periods(..).is_some()` 轻量判据 | safe |
| deep_api.rs:3091-3108,3695,3714,3755 | 同一请求内 `report_source_allows_analysis` 对同一 ds 最多查 3 次 PG（显式 ds 校验、prefetch 判定、report_ds 判定），完全同参重复 | compose_inner 内按 ds 做一次本地 memo（小 HashMap/变量） | safe |
| deep_api.rs:3188-3190 | `let (Ok(ms), Ok(dimensions)) = … else { warn!("PLAN 目录读失败…") }`：let-else 把两个 Err 直接丢弃，warn 里没有任何错误原文，线上无从排查 | warn 带出 `metrics.err()/dims.err()` 原文 | safe |
| deep_api.rs:3211 | PLAN 的 `chat(...).await.ok()?` 静默吞掉 LLM 错误（连 debug 都没有），与 3215 解析失败有 warn 的口径不一致 | 失败时 warn/debug 一记再回退启发式 | safe |
| deep_api.rs:3221-3224,2882 | PLAN_SYSTEM 要求 understanding "最多80字"，代码却截 100 字，提示词与闸门口径不一致 | 统一为 80（或改提示词），带截断测试 | test |
| deep_api.rs:3264-3284,3320-3394 | `s.push_str(&format!(...))` 全文件数十处：每个单元格/每段都先 format! 分配临时 String 再拷贝 | 热路径（table_html 单元格循环）改 `write!(s, …)` 或 push_str 直拼 | safe |
| deep_api.rs:3298,3389 | `bi_page` 参数名 `_evidence` 带下划线前缀（约定=未使用）却在 3389 实际使用，误导读者；`_trust` 才是真未用 | `_evidence` 改名 `evidence`（`_trust` 保留或删参） | safe |
| deep_api.rs:3389-3390,720 | insight 已在 `validate_evidence_insight` 内 sanitize 过一次，`bi_page` 又全量 sanitize 一遍（tokens 重建 + 四次全文 replace），纯重复劳动 | 删掉 bi_page 里的二次 sanitize（或注释说明幂等理由） | safe |
| deep_api.rs:3484 | 板块合计核对用绝对误差 `<= 0.01`：2 亿级指标的浮点/Decimal 换算误差可超 0.01，会把正确报表误标"需复核" | 改 `delta.abs() <= 0.01.max(main.abs() * 1e-9)` 类相对+绝对混合容差，带大数测试 | test |
| deep_api.rs:3594 | 撞活执行器的 409 分支往**共享** PROGRESS 条目 `note(Failed)`：第一个健康运行的进度被写上"处理失败"，且 2162 的 done 判据会因此让前端提前停轮询 | 409 路径不写进度（或只 warn） | test |
| deep_api.rs:3616-3624 | `display_question` 兜底用 `req.question.trim()`，而 `execution_question` 用未 trim 的 `req.question.clone()`，同一问题两份口径 | execution_question 也 trim | test |
| deep_api.rs:3649 | `last_turn(...).await.ok().flatten()` 静默吞掉 PG 错误：上一轮上下文悄悄丢失，追问分诊质量下降且无任何痕迹 | Err 时 debug/warn 一记再按 None 降级 | safe |
| deep_api.rs:3685,3687,4233,4235 | `let _ = save_msg(...)` 四处：会话消息落库失败完全静默（用户消息丢失无日志） | 失败时 warn（文案不带 payload） | safe |
| deep_api.rs:3722,3848,3927,4178 | `req.conv_id.map( | c | c.to_string())…` 同一转换在管线里重复 3-4 次 |
| deep_api.rs:3746 | 用 `e.to_string().contains("无权访问数据源")` 字符串匹配做错误分类，上游文案一改 403 就静默变 422 | 在 agent 侧给错误打类型/标签再匹配（跨 crate，需测试） | test |
| deep_api.rs:3764,3769 | `primary_sales_measure(&execution_question, &primary)` 连续算两遍（内部含 SQL compact 与多指标扫描），3819/3924 的 `expect` 也靠这两遍结果一致才不炸 | 算一次存 `sales_measure` 复用；expect 顺带可降级为 if-let | safe |
| deep_api.rs:3816,3831 | 完全相同的复合条件 `resume.is_none() && sales_report && !is_weekly_report(&req.question)` 写两遍 | 提一个 `let compile_plan = …` 布尔复用 | safe |
| deep_api.rs:3843-3845 | `if let Some(_u) = &understanding` 恒真（3805 已置 Some 默认值），死条件 | 直接 `note(&rid, ProgressStage::Plan)` | safe |
| deep_api.rs:4068 | `let mut svgs = svgs;` 对 4004 已是 `mut` 的绑定做冗余遮蔽重绑定 | 删除该行 | safe |
| deep_api.rs:4085,4115 | 同源比较一处传 `execution_question`、一处传 `req.question`（今日恰好相等），口径分裂隐患 | 统一传 `execution_question` | safe |
| deep_api.rs:4160,3322 | `bi_page` 收到的是 `display_question`（展示文案），却用它跑 `current_period_note` 的周期词识别；展示文案与执行问句不同时周期标注会错 | 周期注记改用 `execution_question` 计算后传入 | test |
| deep_api.rs:4272,4287 | `resume` 在属主校验**之前**就 `note_owner(&rid, &login_name)`：任何知道 rid 的人调一次 resume（拿 403）即把自己登记为该 rid 内存属主，随后 `/api/deep/progress` 属主闸放行——板块标题/断言等经营信息泄漏，恰好架空 2125-2129 声明的防泄露设计 | `note_owner` 移到 4287 属主校验通过之后 | test |
| deep_api.rs:4287-4289 | resume 对非属主回 403、对不存在回 404，响应差异直接泄露"rid 是否存在"，与本文件 2118-2129 自己立下的"统一 404 不泄存在性"纪律不一致 | 非属主也回 404 同形文案（与 progress 端点对齐） | test |

## App.vue（59 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| App.vue:213,2266,2314,2320,2355,2361 | `hasAdminAccess()` 是普通函数却在模板里调用 5 处，每次重渲染都重算 | 改为 `computed` 缓存 | safe |
| App.vue:293,683 | 切换供应商的提示用原始 name（`deepseek`），而列表/备用多模态处用 `providerLabel()` 显示中文名（592,2441-2447），同款两处不一 | 提示语统一走 `providerLabel(name)` | safe |
| App.vue:516 | `name.toLowerCase() === 'dms'` 硬编码内建目标名，魔法字符串 | 提取常量或改用 `t.builtin` 判据 | safe |
| App.vue:790-798 | `resolveFeedback` 无 try/catch，网络异常产生未捕获 rejection 且用户零反馈（同文件其他操作都有「（网络）」兜底） | 包 try/catch，失败 `showToast('反馈状态更新失败（网络）')` | safe |
| App.vue:828,926,2115 | `setTimeout(() => URL.revokeObjectURL(href), 0)` 三处：0ms 回收与下载起动存在竞态（大文件/Firefox 下可能下到空 blob） | 延迟到 ~1000ms 再回收 | test |
| App.vue:970-978 | `onChatClick` 深链拦截不区分 Ctrl/⌘/中键点击，也不查 `e.defaultPrevented`：想新窗口打开的用户被强制进面板 | 仅对无修饰键的左键点击 `preventDefault` | safe |
| App.vue:1106,1166,1408,1465,1479,1680,1711,1854,1937 | catch 里直接 `` `...失败：${e}` ``，用户看到 `TypeError: Failed to fetch` 这类英文原文；同文件别处统一是「（网络）」后缀 | 统一改 `'操作失败（网络）'` 或经 `errMsg` 归一 | safe |
| App.vue:1280 | `validateSession` 的 catch 里 `clearSession()`：后端短暂不可达（网络抖动/重启）就把本地有效会话清掉，用户被迫重登 | 仅在明确 401/403 时清会话，网络异常保留并提示 | test |
| App.vue:1313 | `passwordLogin` 里 `d.roles?.length > 1 ? d.roles : []` 不过滤非字符串；`validateSession`（1276）却做了类型过滤，两处不一 | 统一用同一过滤逻辑 | safe |
| App.vue:1320 | `logout` 未清 `steerText`/`steerCountByConv`/`llmCfg`/`settingsCat`/`quality`/`exemplars`，换账号后旧数据滞留内存 | logout 时一并重置 | safe |
| App.vue:1496 | `rememberSession(tm[1], '企微用户')` 对 hash 里的 token 未做 `decodeURIComponent`，含保留字符的 token 会取错 | 取 `decodeURIComponent(tm[1])` | test |
| App.vue:1549 | GET `/api/artifact/list` 用 `authHeaders()` 带了无谓的 `Content-Type: application/json`；同文件其他 GET 均用 `authHeaders(false)`（890,1215,1377,1425） | 改为 `authHeaders(false)` | safe |
| App.vue:1691 | `offerRoles` 里 `r.roles |  | []` 不做字符串过滤（对比 1276），脏数据会渲染成空按钮 |
| App.vue:1779-1785,1806 | 普通提问的引用 chip 在 1806 才 `splice` 出来，`retryOptions`（1779 构建）里没有 `refs`：发送失败点「重试」（2617）引用静默丢失 | 把 `refs: sendRefs` 回填进 `aiTurn.retryOptions` | test |
| App.vue:1873-1883 | `setInterval(async …)` 轮询无 in-flight 闸：一次请求超过 1.2s 就并发叠请求 | 加在飞标志，未回来就跳过本拍 | test |
| App.vue:1903,2617 | 续跑失败后 `t.resumable` 已置 false，错误气泡只剩「↻ 重试」，点它是拿 `retryQuestion` 发起全新 run 而非断点续跑，语义跳变用户无感知 | 续跑失败后重新 `checkResumable`（仅 409 做了，1925），或重试按钮文案区分 | test |
| App.vue:1950-1952 | `secSemantic` 只是 `semanticForLabel` 的零价值转发 | 内联删除，调用处直接用 `semanticForLabel` | safe |
| App.vue:1957-1960 | `isGrossMarginValueLabel` 只认精确「毛利率/销售毛利率」，「毛利率（%)」「毛利率（整体）」等变体走不到百分分支 | 放宽为前缀/包含匹配并补用例 | test |
| App.vue:1979 | `secY` 对单列板块返回 `[1]`，列下标越界传给 BiChart | `Math.min(1, sec.columns.length - 1)` 钳位或空列早退 | test |
| App.vue:2178 | `deep: t.mode === 'deep' |  | null` 布尔短路出 null 的写法晦涩 |
| App.vue:2204 | `if (!t.convId)` 用 truthy 判数字 id——正是本文件 1397-1399 长注释点名要消灭的形态 | 改 `t.convId == null` | safe |
| App.vue:2223 | `conv_id: String(t.convId)` 传字符串，而 1815/1919 传 number，同一字段两种类型 | 对齐契约统一类型（看后端实际解析） | test |
| App.vue:2257 | `:title="'明暗切换'"` 静态字符串用了动态绑定 | 写 `title="明暗切换"` | safe |
| App.vue:2289,2290,2335,2346,2747,2748,2995,2996 | 纯图标按钮（🕓/×/✕/☰/↓/⛶）只有 `title` 没有 `aria-label` | 补 `aria-label` | safe |
| App.vue:1499,2294 | 健康检查只在 onMounted 跑一次，状态永远停留首屏那一刻；侧栏健康灯不可点 | 允许点击重查或定时轮询 | safe |
| App.vue:2396 | 「密码留空时保留原密码」在新增模式（dbEditor==='new'，密码必填）也显示，误导 | 仅 `dbEditor === 'edit'` 时显示该 tip | safe |
| App.vue:2471 | 「Key 留空时保留已存值」同理在新增供应商时也显示（llmFormValid 要求新增必填 key） | 仅编辑态显示 | safe |
| App.vue:2419 | 数据库「保存」按钮没有 `:disabled="dbSaving"`（LLM 保存 2487 有），保存中可重复点击 | 补 `:disabled="dbSaving"` | safe |
| App.vue:2423 | 连通成功文案硬编码「MySQL ${version}」，数仓类型实际是 Doris | 按 `dbForm.type` 区分文案或去掉产品名 | safe |
| App.vue:2475,610-619 | 预设下拉里 `custom` 选项在 `onPreset` 中查不到对应 preset 直接 return，选了无任何反应 | 选中 custom 时清空/保留表单并给占位提示 | safe |
| App.vue:2463 | 「切换」按钮因 `!p.key_ready` 被 disabled 时没有任何 title 解释原因 | 加 `title="key 未配置，请先配置"` | safe |
| App.vue:2495 | key 删除按钮 disabled 的 title 只解释 protected 一种情况，主模型/备用占用时 title 仍写「删除该 Key」 | title 按禁用原因动态给出 | safe |
| App.vue:2525 vs 2545 | 「暂无反馈」无句号、「暂无 SQL 样例。」有句号，空状态文案不一致 | 统一标点 | safe |
| App.vue:2527 | 反馈「处理/重开」无 busy 态，双击发两个 POST（setExemplarStatus 有 exemplarBusy 闸，773） | 加 busy 禁用 | safe |
| App.vue:2592 vs 2603 | 深度 loading 写「正在分析」、精简写「分析中…」，同一状态两种说法 | 统一措辞 | safe |
| App.vue:2613 | 角色选择按钮无 in-flight 禁用，`pickRole` 里两个连续 await，双击会重复换签 | 加 busy 标志禁用按钮排 | test |
| App.vue:2622-2624,2659-2661 | `<a>` 产物卡内嵌 `<button class="art-share">`，嵌套交互元素（HTML 非法/a11y 差），靠 prevent/stop 兜底 | 把分享按钮移出锚点或改为同级 | safe |
| App.vue:2639,2642,2644,2819 | `margin-left:auto` 靠四个互相咬合的内联 style 三元表达式维持右对齐，深度无 page 的边角下按钮不再靠右 | 在 `.res-meta` 用 CSS（如 `:first-child`/flex 规则）统一 | safe |
| App.vue:2672,2735 | `t.page.sections.length` / `v-for … t.page.sections` 直接访问；`DeepPage.sections` 声明为必填但深度页其他字段全都按可选防御，老服务端缺键即整轮崩溃 | `t.page.sections?.length` + `v-for="(sec,si) in t.page.sections ?? []"` | test |
| App.vue:2519,2764 | 两处表格首行 `<tr><th>` 直接放 `<table>` 下无 `<thead>`（2753 用了 thead），同款两处写法不一且 sticky 表头语义缺失 | 统一包 `<thead>` | safe |
| App.vue:2778 | `v-if="t.result.truncation_note"` 检查了服务端字段却渲染硬编码文案，服务端给的「原因/范围/续读参数」（接口注释 64-65）被丢弃 | 渲染 `{{ t.result.truncation_note }}` | test |
| App.vue:2790-2793 | `knowledgeSources(t.result)` 同一次渲染调用 3 遍（每遍重建 Set）；2811/2813 `compoundAnalysis` 2 遍；2833/2835 `clarifyOptionsOf` 2 遍——每次任意响应式变动都重算 | 每轮一次算好挂到计算属性/局部缓存 | safe |
| App.vue:2819-2820 | 「生成报表」用无 href 的 `<a>`，不可 Tab 聚焦、无键盘触发 | 改 `<button type="button">` | safe |
| App.vue:2822-2824 | 分析面板的报表产物卡没有分享按钮，而 2622/2661 两处同款产物卡都有 🔗 | 补齐同款分享按钮 | safe |
| App.vue:2867-2869 | mode-seg 在知识库模式下仅置灰 CSS，按钮仍可聚焦可点击（静默无操作） | 加 `:disabled="intent === 'knowledge'"` | safe |
| App.vue:2871 | textarea `rows="1"` + CSS `max-height:160px`，但无任何自动增高脚本，max-height 永远不生效 | 输入时按 scrollHeight 调高或去掉 max-height | safe |
| App.vue:2898 | toast 浮层无 `role="status"`/`aria-live`，读屏器感知不到操作反馈 | 加 `role="status" aria-live="polite"` | safe |
| App.vue:2932 | 登录遮罩无 `role="dialog"`/`aria-modal`（trace 抽屉 2332、周报 2901 都有） | 补齐 dialog 语义 | safe |
| App.vue:2971 | `(v.created_at |  | '').slice(5, 16)` 切出 `08-10T19:03`，ISO 的 `T` 直接露给用户 |
| App.vue:2981 | 预览 `<iframe>` 无 `title` 属性（openPreviewWindow 里 847 设置了，两处不一） | 加 `:title="preview.title"` | safe |
| App.vue:2985 | bi-focus 沉浸层只能点遮罩/✕ 关闭，无 Esc（trace 抽屉 1229、周报 2900 都支持 Esc） | 打开时挂 keydown Esc，关闭时摘 | safe |
| App.vue:2755,2765,3004 | 表格体 `v-for="(_, ci) in r"` 按行数据长度出单元格，后端少给列时行与表头错位 | 按 `columns` 迭代、用 `r[ci]` 取值 | test |
| App.vue:2108-2109 | CSV 导出未防公式注入：以 `=`/`+`/`-`/`@` 开头的单元格原样写出，Excel 打开即执行 | 命中前缀时前置 `'` | test |
| App.vue:3447 | `var(--warn-text, #b45309)` 变量名笔误——theme.css 只定义了 `--warning-text`（grep 全仓无 `--warn-text` 定义），永远走兜底且暗色主题不适配 | 改为 `var(--warning-text)` | safe |
| App.vue:3163-3164,3217,3403-3408 | `.mini-inp`、`.insight`、`.bi-contract`、`.bi-under` 在全仓模板中零引用（grep `class=` 无匹配），疑似死样式 | 确认后删除 | safe |
| App.vue:3218-3219 | 两个相邻的 `@media (max-width: 760px)` 块；3502 与 3537 两个 `@media (max-width: 820px)` 块 | 合并同断点媒体查询 | safe |
| App.vue:3423-3424 | `.dk-compare.up/down` 硬编码 `#c93b32`/`#16845b`，同文件涨跌色别处用 `var(--error-text)`/`var(--success-text)`（3293-3294），暗色主题下这两色不跟随 | 换用 CSS 变量 | safe |
| App.vue:868,952 | toast 文案带 emoji（🔗/📌），其余 toast 全部纯文字，反馈风格不一 | 统一去 emoji 或统一加 | safe |
| App.vue:1637-1659 | 周报生成绕过排队追问机制：队列里已有待发问句时周报直接插队先跑（send 带参调用不排队，1062 注释自认） | 周报也入队或提示「将在排队问题后生成」 | test |

## web/src/ResultPanel.vue（55 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| web/src/ResultPanel.vue:26-27 | 注释说「这里三处解引用」，但模板实际解引用 `result.view` 处已增至 429/548/569/606/706 等多处，数量过时 | 去掉具体数字或改为「多处」 | safe |
| web/src/ResultPanel.vue:74 | `compositionCharts` 用 `b.kind !== 'line'` 过滤，`kind` 缺失的畸形 chart block 也会落入，随后 569 行 `b.kind!` 把 `undefined` 传给 BiChart，567 行标签还显示「排名」 | 过滤改为 `b.kind === 'bar' \ | \ |
| web/src/ResultPanel.vue:120,153 | 魔法字符串 `'实体候选匹配：'` 硬编码两处，任一处改动即静默失效 | 提为模块级常量共用 | safe |
| web/src/ResultPanel.vue:136,440 | 若后端选项被 `intentOptions` 正则全部过滤（如只返回「其他」），`.ask-opts` 仍渲染空容器 | 给 `.ask-opts` 加 `v-if="intentOptions.length"` | safe |
| web/src/ResultPanel.vue:144,145 | 宽表阈值 `> 3` 魔法数字在主表/补充表各写一份 | 提常量 `WIDE_TABLE_COLS = 3` | safe |
| web/src/ResultPanel.vue:229,231 | 正则交替 `证据\ | 证据编号` 冗余（`证据` 前缀已覆盖后者）；231 行 `/i` 对纯中文关键字无意义 | 精简正则，去掉冗余分支与 `/i` |
| web/src/ResultPanel.vue:267 | `clipInsight` 用 `slice(0, 75)` 截断，可能切断 emoji/代理对产生乱码 | 改用 `Array.from(text).slice(0,75).join('')` 或 Intl.Segmenter | safe |
| web/src/ResultPanel.vue:299 | `deltaText` 未防 `d.pct` 为 NaN/Infinity（后端异常时 Intl 输出 "NaN" 直接上屏），而 304 行 baseline 已有 `Number.isFinite` 判法 | 同样加 `Number.isFinite(d.pct)` 守卫 | safe |
| web/src/ResultPanel.vue:299 | 百分 delta 单位文案 `pp` 对中文业务用户不直观 | 改「个百分点」或加 `title="百分点"` | safe |
| web/src/ResultPanel.vue:302-311,518,644 | `deltaDetail(k)` 在模板里被调两次（`v-if` + 插值），每次渲染重复字符串格式化；主区/补充区各一份 | 预算为视图模型数组，模板只读属性 | safe |
| web/src/ResultPanel.vue:309 | `delta.change` 非有限数时回落半角 `'-'`，渲染成「变化额 -」易被读作负号 | 改全角 `'—'` | safe |
| web/src/ResultPanel.vue:330-331 | `entityTitle` 客户分支正则含 `storecode\ | storename`，门店实体标签带 storename 会先命中客户分支被误判为「客户档案」，331 行门店分支永远够不到它 | 从客户分支移除 `storecode\ |
| web/src/ResultPanel.vue:364 | `shownRows = computed(() => props.result.rows)` 是无附加值透传，长注释的智慧都在「不 slice」上，computed 本身可省 | 模板直接用 `result.rows` 或内联 | safe |
| web/src/ResultPanel.vue:365-367,490 | rowFoot 注释称「『后面还有』那句由后端的 `truncation_note` 说」，但 490 行只渲染固定文案、从不显示 `result.truncation_note` 原文——注释与代码不符，后端三件套（原因/范围/续读）信息丢失 | 渲染 `result.truncation_note` 原文（固定文案作兜底），或修正注释 | test |
| web/src/ResultPanel.vue:373 | `rowFootFor` 的 `Pick` 含 `row_count` 但函数体只用 `rows.length`，类型签名误导读者 | 从 Pick 中去掉 `row_count` | safe |
| web/src/ResultPanel.vue:373-378 | `pointsToNotice` 参数导致同款截断脚注两种文案：主表「当前展示部分数据」、补充表「部分数据」 | 统一为一句文案，去掉参数 | safe |
| web/src/ResultPanel.vue:401-402,406 | `cellFor`/`cellTitleFor` 直接 `result.rows[ri][ci]`，后端若返回 null 行（畸形 JSON）即 TypeError 白屏 | 改 `result.rows[ri]?.[ci]` | safe |
| web/src/ResultPanel.vue:429 | 空视图文案「该结果没有视图（view 缺失），无法按表格呈现」是开发者措辞，终端用户可见 | 改用户向文案，如「该结果暂不支持表格化展示」 | safe |
| web/src/ResultPanel.vue:439 | `.ask-q` 无 `v-if`，`caliber_note` 缺失时渲染空 div 占位 | 加 `v-if="result.caliber_note"` | safe |
| web/src/ResultPanel.vue:444-455 | 自定义问法 input 无 `maxlength`，超长文本可直发后端 | 加 `maxlength`（如 200） | safe |
| web/src/ResultPanel.vue:458 | 提示「按 Enter」在移动端无 Enter 键场景不成立，且与「提交」按钮并存 | 文案改为「选择一个问法，或输入自己的问题」 | safe |
| web/src/ResultPanel.vue:471 | entity-choice `:key="choice.query"`，后端 drill 返回重复问法时 key 冲突 | key 改为 `` `${choice.query}-${index}` `` | safe |
| web/src/ResultPanel.vue:484,487,490,493,499 | caliber-warn/derive-note/trunc-note/redact-note/empty-hint 五类提示条均无 `role`，屏幕阅读器不会主动播报 | caliber-warn 加 `role="alert"`，其余加 `role="note"` | safe |
| web/src/ResultPanel.vue:497-499 | 空数据条件排除 `business-lookup`，但上方注释只提 need-intent/no-topic 与 entity-card 三类，注释漏一族 | 注释补上 business-lookup 及原因 | safe |
| web/src/ResultPanel.vue:500 | 文案用全角空格「记录　②」做排版，字体/主题下宽度不稳定 | 改普通空格或「；」分号分隔 | safe |
| web/src/ResultPanel.vue:513,639 | KPI 值 `displayValue(...)` 返回空串时无兜底（entityValue 273 行、cellFor 403 行都有 `\ | \ | '—'`），空值 KPI 卡数值区空白 |
| web/src/ResultPanel.vue:515,641,794-795 | 涨跌仅靠红/绿色+小箭头区分（红涨绿跌），色弱用户难辨；且「下降=绿色 success」对销售额类指标语义错位 | 箭头加 `aria-label`（升/降/平），颜色语义注释说明是中式涨红跌绿约定 | safe |
| web/src/ResultPanel.vue:548,569,656,669 | `b.x!`、`b.y!` 非空断言掩盖缺字段的畸形 block，x/y 为 undefined 时 BiChart 收到 undefined | 渲染前过滤 `b.x !== undefined && b.y?.length` 的 block | test |
| web/src/ResultPanel.vue:567,667 | chart-type 标签非 pie 即「排名」，bar 图未带 `top`（未排名）时名不副实 | 按 `b.top != null` 区分「排名/对比」 | safe |
| web/src/ResultPanel.vue:592,687 | 主表守卫用 `row_count > 0`，补充表用 `rows.length && columns.length`——同款判断两种写法；row_count>0 而 rows 为空时主表渲染空 tbody | 统一为 `rows.length > 0` | safe |
| web/src/ResultPanel.vue:605,691 | 数据列 `<th>` 缺 `scope="col"`（仅行号列 604 行有） | 补 `scope="col"` | safe |
| web/src/ResultPanel.vue:606 | 🔒 表情无无障碍文本，读屏只念「锁」或无输出 | 加 `role="img" aria-label="本列已脱敏"` | safe |
| web/src/ResultPanel.vue:611,693 | `v-for="(row, ri)"` 中 `row` 未使用（614/695 行已用 `_` 占位），写法不一且产生未用变量 | 统一改 `(_, ri)` | safe |
| web/src/ResultPanel.vue:615-617 | 每格重复调用 `isRedacted(ci)`×3、`isMetric(ci)`，且 `cellTitle` 与 `cell` 对同一值各做一次 displayValue 格式化（200 行 × N 列翻倍） | 列级预算 redacted/metric 数组，格级文本算一次复用 | safe |
| web/src/ResultPanel.vue:623,702 | 「悬停单元格可查看完整内容」只在 ≤720px 窄屏显示（868+1007 行），窄屏多为触屏没有悬停；且两条提示文案重复两处 | 触屏文案去悬停半句或改「点击单元格」；文案提为常量复用 | safe |
| web/src/ResultPanel.vue:636-646 | 补充区 KPI 卡与 511-519 主 KPI 卡逐行重复（含 deltaDetail 双调用问题被复制两份） | 抽 `KpiCard` 子组件复用 | safe |
| web/src/ResultPanel.vue:651-670 | 补充区趋势/构成图表卡与 540-570 主图表卡逐行重复（仅 columns/rows 来源不同） | 抽 `ChartCard` 子组件复用 | safe |
| web/src/ResultPanel.vue:706,708 | 钻取区直接用 `result.view.interact?.drill`，而 92 行已有 `drillOptions` computed，同一数据两条访问路径 | 模板统一用 `drillOptions` | safe |
| web/src/ResultPanel.vue:708 | drill pill 用 `<span @click>`：不可聚焦、无键盘事件、无 role，与 441 行 ask-opt 用 `<button>` 不一致 | 改 `<button type="button" class="pill">` | safe |
| web/src/ResultPanel.vue:708 | pill 点击无 `.stop`，与组件内其它可点元素（441/443/474）写法不一 | 统一补 `.stop` 或注释说明不需要 | safe |
| web/src/ResultPanel.vue:708 | 「按{{ d }} ↓」的 ↓ 符号暗示展开面板，实际是发起新查询跳转 | 换 `→` 或去掉符号 | safe |
| web/src/ResultPanel.vue:57,731-734 | `trust` 接口声明了 `trace_id`、`route` 字段，但核查详情只渲染 level/source/access/execution/fingerprint，排障最有用的 trace_id 不可见 | 核查详情补「Trace {{ auditTrust.trace_id }}」 | safe |
| web/src/ResultPanel.vue:62,94,97-102 | 62 行注释「恒单行五值」与 94 行注释「固定四格」及 SALES_CONTEXT_CELLS 实际 4 格互相矛盾 | 统一两处注释口径 | safe |
| web/src/ResultPanel.vue:113,283 | 同窗毛利率 `(n*100).toFixed(2)`（2 位小数）与 displayValue 的 `fmt(n*100,'percent')`（1 位小数，format.ts:60）两处毛利率精度不一致 | 统一精度（同窗卡也走 displayValue 或 fmt） | test |
| web/src/ResultPanel.vue:276-287,296-300 | 百分比双口径：值显示按标签名判毛利率 ×100，delta 是否按 pp 却看 `semantic==='percent'`——semantic 为 none 的「毛利率」KPI 会出现值 ×100、delta 按相对 % 的错配 | 值与 delta 用同一判据（同一函数出口） | test |
| web/src/ResultPanel.vue:742-758,945 | insight-grid 固定 `repeat(3, …)`，只有 1-2 张卡时卡片被压成 1/3 宽、留白难看 | 改 `repeat(auto-fit, minmax(220px, 1fr))` | safe |
| web/src/ResultPanel.vue:762-999 | 组件核心样式依赖 App.vue 全局（.empty-hint/.caliber-warn/.trunc-note/.redact-note/.ask-card/.pill/.num/.tbl-wrap 的 nowrap 与 border-bottom，App.vue:3233-3310），scoped 只覆盖一半——样式双源，删全局即静默破损且无处声明此依赖 | 关键样式搬入 scoped，或在文件头注释声明对 App.vue 全局类的依赖 | safe |
| web/src/ResultPanel.vue:857,865 | 斑马偶数行数据格用 `color-mix(…56%…)` 底色，同行 sticky 行号格却用纯色 `var(--bg-main)`，一行两种底色 | 行号格改用同一 color-mix | safe |
| web/src/ResultPanel.vue:961-999 | ask-custom/ask-input/ask-submit 样式用多行展开写法，与文件其余单行紧凑风格明显不一（且同族 .ask-card 样式在 App.vue） | 排版风格与存放位置对齐文件惯例 | safe |
| web/src/ResultPanel.vue:1002,1004,1008,1018,1021,1025 | @container 720 与 @media 600 两断点重复定义相同规则（chart-grid.paired/entity-grid/supplemental-head 各两份），无注释说明是兼容回退 | 去重或加注释说明双断点意图 | safe |
| web/src/ResultPanel.vue:1026 | `.supplemental-head .row-count { padding-top: 17px }` 是死样式：模板中 `.supplemental-head`（628-634 行）内不存在 `.row-count`，行数在 688 行 `.supplemental-subhead` 里且无该类名 | 删除该规则 | safe |
| web/src/ResultPanel.vue:177,218 | insightCards 对 `insightText.value` 再 `replace(/\r/g,'')`，而 218 行 sanitizeInsight 已去过 `\r`，双重处理冗余 | 删 177 行的重复 replace | safe |
| web/src/ResultPanel.vue:314,322 | `block.x ?? -1` 用 -1 哨兵下标取 `columns[-1]` 拿 undefined 兜底，可读性差 | 改显式判空 `block.x === undefined ? undefined : view?.columns[block.x]` | safe |
| web/src/ResultPanel.vue:5 | BiChart `defineAsyncComponent` 无 loading/error 组件，弱网/chunk 失败时图表区长期空白无反馈 | 加 `loadingComponent` 占位与 `errorComponent` 兜底 | safe |
| web/src/ResultPanel.vue:343 | 自定义问法提交后立即清空输入，父级发送失败时文本丢失无法重试 | 失败时由父级回填，或监听发送结果再清空 | safe |

## direct.rs（55 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| direct.rs:36-51 | 文档注释错位：36-45 行「诊断为什么没走确定性装配」整段是 `why_not_compose` 的文档，却挂在 `hardcoded_producer` 上（rustdoc 会把它显示给后者） | 把 36-45 行移到 `why_not_compose`（73 行）头上 | safe |
| direct.rs:53,70 | `hardcoded_producer`/`doc_binding_hit` 硬编码 `sniff_doc_code(question, true)`：非数仓问句诊断时，拆单号（`_2` 后缀，5111 行钉住生产不识别）会被误报「单号直查」兜底，诊断与实际行为漂——正是本文件反复警告的形态 | `why_not_compose` 增加 warehouse 形参并透传 | test |
| direct.rs:111-115,232-234 | `pick` 选中指标后，`metric_word` 用同一个 `match_word` 对同一指标**再算一遍**；`pick` 内部已算出命中词却丢弃 | `pick` 返回 `(def, word)`，调用点直接用 | safe |
| direct.rs:249-259 | `compose_gated` 文档首段仍以「一律不装配」开头描述旧行为，259 行才用「⚠️ 不再一律拒」收回；读者先读到的是已死行为 | 重写文档首段直接描述现行「按声明装配」，旧行为降为历史注记 | safe |
| direct.rs:271 vs 424-427,447-458,473,619,701,793 | 大小写判据不一：快照匹配用 `eq_ignore_ascii_case`，而 `find_path`/`find_edge`/`left_join` 的 scope 查找、619/701/793 的表名比较全是 `==`。注册表一旦出现大小写漂移，路径找不到/表级口径漏挂（后者就是 41% 虚增的失败面） | 统一为 `eq_ignore_ascii_case`（表名比较集中一个 helper） | test |
| direct.rs:378 | `pseudo` 闭包里 `question.replace(taken, "")` 对每个维度候选词都重新分配一次整句字符串 | 在 `filter_map` 外预计算一次 `question_without_taken` | safe |
| direct.rs:424-441 | `find_path` 每层 `for e in edges` 全表扫 + `path.clone()`；注册表边数小今天无感，但 `visited.insert(next.clone())` 又一次克隆 | 克隆可省（`p.push` 后 `queue.push_back((next.clone(), p))` 已有一次）；注释说明规模假设即可 | safe |
| direct.rs:460-461 | 文档写「测试与旧调用点用」，但函数是 `#[cfg(test)]`——不存在旧调用点，文档与代码不符 | 删掉「与旧调用点」 | safe |
| direct.rs:528-531 | `to_uppercase().contains("SELECT")` 会误中 `'SELECTED'` 类字面量（过度拒，安全方向）；而 `" UNION "` 要求 UNION 后必须是空格，`UNION\nALL`（换行）会从网眼漏掉。两处各 `to_uppercase()` 分配两次 | 词边界判定（split_whitespace 含 "UNION"/"SELECT" 词元）一次归一后比较 | test |
| direct.rs:540-541,566 vs 270,292 | 维度侧三处不一致：① `d.source_table` 不过 `strip_annotations`（指标侧 525-527 过了）；② 基表用 `split_whitespace().next()` 而 292 行用 `first_ident_of`——维度声明若带 `(JOIN …)` 注解，`dim_base` 会取出 `t_x(JOIN`；③ 566 行同基表分支 `from = d.source_table.clone()` 把未剥离注解原文拼进 SQL | 维度侧与指标侧同走 `strip_annotations` + `first_ident_of` | test |
| direct.rs:543-547 | `splitn(3, char::is_whitespace)` **不合并连续空白**：声明里两个空格会让 `dim_rest` 错含别名（`"t  cus JOIN…"` → dim_rest=`"cus JOIN…"`，FROM 拼出 `t cus cus JOIN`）。与 540-541 的 `split_whitespace` 行为不一 | 改用 `split_whitespace` 收集后取 `skip(2).join(" ")` | test |
| direct.rs:575 | `m_agg.to_uppercase().starts_with("COUNT(DISTINCT")` 未先 `trim`：声明前导空格会让扇出检查失效（SUM 沿 1:N 扇出虚增的防线被空格绕过） | `m_agg.trim().to_uppercase()` 或 trim 后再判 | test |
| direct.rs:617-633 | 先 `fill_time_col(&tpl, "order_time")` 再 `p.replace("order_time", "{alias}.order_time")`：子串替换会把模板里任何含 `order_time` 的标识符（如 `prev_order_time`）改坏；且填了再换是两次活 | 先定别名，再 `fill_time_col(&tpl, &format!("{alias}.order_time"))`，删掉 replace | test |
| direct.rs:650,662 | 值过滤桥接时 `from_table_aliases(&from)` 在每个 vf、每一跳都重扫一遍 FROM 串 | 循环外算一次，桥进新表后增量 push | safe |
| direct.rs:815 | G2 用 `p.contains(col_ref)` 子串判冲突：`b0.qty` 会被 `b0.qty_total` 误中（过度拒，安全方向但无注释说明这是刻意的宽判） | 加一行注释说明「子串误中 = 安全方向的过度拒」，或改词边界 | safe |
| direct.rs:966 vs 2415 | `metric_only` 的让路门再跑一遍 `agg_template(question)`，而 `compose_hit` 的让路门（2415）刚刚跑过同一个问句同一个函数；`agg_template` 本身是 ~40 词全句 replace 的重扫描 | `compose_hit` 把让路判定结果传给 `try_compose_metric_only` | safe |
| direct.rs:1021-1031 | prev/yoy 两条各完整重跑 `compose_sql_with_snap`（含 `value_filters` 全表扫 + 残留守卫），每问句最多装 3 遍；注释已论证「同一装配」是刻意的，但 vfs/残留判定结果可缓存复用 | 抽出可复用的中间结构，三次只换时间模板（保持「只差时间窗」断言 3596-3605 继续绿） | test |
| direct.rs:1072 | `value_filters` 早筛先 `v.name.chars().count() >= 2` 后 `question.contains(...)`：`contains` 才是选择性条件，936 行逐行先数字数再 contains，顺序反了 | 两条件互换（`&&` 短路） | safe |
| direct.rs:1148 vs 1561-1583 + lexicon.rs:44 | 「最低」半接线：`STRIP_WORDS` 有最高/最多/最少/最大/最小**独缺最低**，`has_entity_residue` 在 1148 手工补了 `"最低"`，但 `warehouse_sales_fact_predicated` 的 consumed（1561-1582）没补——「本月销售额最低的客户」残留「最低」→ 落「未确认限定」卡，而 1590/1594 的 ASC 排序分支为它白写着 | consumed 构造处同样补「最低」（或 kernel 词表补齐，走 kernel 侧纪律） | test |
| direct.rs:1588-1589 vs 326-332 | 「最低 N 个」的 N 丢失：`detect_top_n`（kernel/time.rs:57）极值词表不含「最低」，`ranking_limit` 在 326-332 用 replace 打了补丁，但 sales_fact 路径直接用 `detect_top_n`——「销售额最低的 5 个客户」得 ASC LIMIT 200 而非 5 | 1588 行改用 `ranking_limit(question)` | test |
| direct.rs:318-324,1590 | 「最差」两不管：`rank_direction` 只认最少/最小/最低，ranking 词表（1590）有「最好」无「最差」——「销售额最差的 5 个客户」不触发排序降序方向也不触发 ranking | 与产品确认后按 最好/最差 对称补齐 | test |
| direct.rs:1162 | `(q.contains("还买")‖"还购买"‖"关联购买"‖"一起买") && q.contains("买")`：四个析取项字字都含「买」，合取恒真——死条件 | 删 `&& q.contains("买")` | safe |
| direct.rs:1186-1196 | `strip_relation_words` 末段单字词（有/的/是/都/买）无位置剥离：实体名含这些字被吃掉——「买过**美的**冰箱的客户」剥完剩「美冰箱」，探库/过滤全错 | 单字词只在边界剥（首尾或紧跟疑问词），或限制为「实体名 ≥2 字且剥后不短于原名一半」类判据 | test |
| direct.rs:1296,1338 | `question.find(*word).unwrap_or(usize::MAX)`：word 上游已 `filter(contains)`（1266/1324 链），`find` 恒 Some，兜底分支是死的 | 改 `unwrap_or_default()` 或注释说明防御意图 | safe |
| direct.rs:1355-1362 | `WAREHOUSE_SALES_UNSUPPORTED` 缩进畸形（整体多缩 4 格）；且收了拼错的 "manger" 却没收正确的 "manager" | rustfmt 缩进；按实证决定是否补 "manager" | safe |
| direct.rs:1358 | 补 "manager" 属行为变更（多拦一类问句进失败关闭卡），与上一条分开 | 加词 + 回归断言 | test |
| direct.rs:1437-1455 vs 1561-1582 | `unrecognized_residue` 把 `warehouse_sales_fact_predicated` 的 consumed 构造（指标名/别名/extras + 维度名/别名/extras + tail words + scalar 判定）**逐行抄了第二份**——本文件自己最反对的「两份会漂」 | 抽 `sales_fact_consumed(question) -> (Vec<String>, bool/*scalar*/)` 共用 | safe |
| direct.rs:1506-1507 | 先转义后截断：`replace('\'',"''")` 把引号翻倍后 `chars().take(20)` 可能把 `''` 对劈开，留奇数引号 → 「不可计算」卡 SQL 语法错误（兜底卡自己挂掉） | 先 `take(20)` 再转义 | safe |
| direct.rs:1603-1608 | `else if time_dimension.is_some() && !ranking { Some(Sort::dimension(time_dimension.unwrap(), …)) }`：is_some 后 unwrap | 改 `if let Some(td) = time_dimension` 链 | safe |
| direct.rs:1685,1702,1719 | 关系 SQL 只转义 `'` 不转义 `\`：实体名以 `\` 结尾时 `\` 吃掉闭引号 → 语法错误；且 LIKE 里实体名含 `%`/`_` 被当通配符（语义放宽，无人察觉） | 与 `sales_fact::quote`（sales_fact.rs:431）同规格转义；或改用 INSTR 子串语义 | test |
| direct.rs:1766-1801 | `warehouse_market_cost` 不管 rank 与否都先拼 `total` 与 `prev`，rank 时两者直接丢弃（1799/1801） | rank 分支短路，total/prev 惰性构造 | safe |
| direct.rs:1791,1856,1952 | rank 触发词含裸「前」：「**目前**市场费用」「之**前**的账户余额」误中 → 该出总额的问句出成分类明细。「前」在 `detect_top_n` 里有时间单位黑名单护航，这几个裸 contains 没有 | 「前」改为「前+N/前十」形态判定（复用 detect_top_n 的结果 >0 或 ≠200） | test |
| direct.rs:1791,1856,1952 | "top"/"TOP" 两写法枚举但 contains 大小写敏感，"Top" 漏；kernel 的 detect_top_n（time.rs:38）是先 lowercase 的 | 统一 `question.to_lowercase().contains("top")` 一次 | safe |
| direct.rs:1905-1910 | `stock_province_predicate` 对每个省（33 个）都 `format!` 拼 7 个短语再 find——问句不含任何省时白做 ~231 次分配 | 先 `if !code_hit && !question.contains(name) { continue; }` 再拼 phrases | safe |
| direct.rs:2014-2020 | 词表冗余：「下过单/有下单/有过下单」都含「下单」，「有那些客户」含「那些客户」——contains 语义下后四个词永不独立生效 | 删冗余项（逐字等价） | safe |
| direct.rs:2077-2079,2103-2105,2113-2115 | `device_orders` 里 `time_predicate(question)` 最多调 3 次（2077 一次 + 两个分支各一次），且 2077 的 `time` 在前两个分支根本不用 | 函数入口算一次 `time_predicate`，各分支 fill 不同列 | safe |
| direct.rs:2081-2090 vs 2182-2195 | 同一张 16 臂 order_status CASE 映射抄了两份（device_orders 内联 vs `sales_status_sql`），漂移即两处状态文案不一致 | `sales_status_sql` 形参改列名（传 "o.order_status"/"order_status"），device_orders 复用；现有断言（3310-3314）已钉字节 | safe |
| direct.rs:2145-2147 | `sales_breakdown` 是 `sales_breakdown_for` 的空壳转发，无任何注释说明为何存在两层 | 内联为一层，或注释「保留公开名给 X」 | safe |
| direct.rs:2176 vs 3117-3119 | 标点白名单两套：`residual_text` 的过滤串没有「」『』（），而 `customer_name_fragment` 的 trim 集有——带书名号/括号的问句一边算残留一边不算，判据不一 | 两处共用同一个常量 | safe |
| direct.rs:2425-2427 + 944,977 | `compose_hit` 先 `try_compose`（7 张注册表 join! 全读），None 后 `try_compose_metric_only` 又重读 6 张——指标 only 的问句每题白付 6 个 PG 往返（202-204 行的注释正是为消这个而写的，只消了一半） | `try_compose` 返回携带已读注册表的枚举，或合并入口一次读齐 | test |
| direct.rs:2837 | `is_measure_col` 用 `c.contains("cost")`：`mat_costume`（服装，1746 行就在本文件词表里）这类列被误判度量列——闸 1 通道③的放行面比注释写的宽 | 词元切分后判（`_` 分隔段等于 cost/quantity…），或至少注释知悉 | test |
| direct.rs:2860-2863 | 闸 1 通道①：`label.contains(cmt)` 方向对**短注释**太宽——注释若只写「金额」，「开票金额」就被放行（E05 的虚构形态复活）；今天靠注释写得长（「明细金额（应付金额）」）碰巧安全 | `label.contains(cmt)` 加最小长度门槛（如 cmt ≥2 汉字且 ≠ 通用度量词） | test |
| direct.rs:2862 | `col.contains(label)`：列名全 ASCII、label 必含 CJK（2695 行保证），该子条件恒 false——死判据 | 删除或注释「防未来 CJK 列名」 | safe |
| direct.rs:2866-2871 | 通道②按空白/`/` 切 source：注解形态 `t_x(JOIN t_y …)` 切出 `t_x(JOIN`，与裸表名永远不等——带注解的注册指标走不通同源通道（安全方向，但与 270 行 `first_ident_of` 的判据不一） | 段内先 `first_ident_of` 再比 | test |
| direct.rs:2919-2922 | `bare_table` 先 trim 两端引号再 rsplit('.')：`` `db`.`tbl` `` 形态 trim 后得 `` db`.`tbl ``，rsplit 出 `` `tbl ``——残留反引号导致等值比较永不命中，datamap 若以引号限定名存边则证据全失效 | rsplit 之后再 trim_matches | test |
| direct.rs:2948-2951 | `reply.content.as_deref()?`：content 为 None 时静默提前返回（无 warn），只有「有 content 但抽不出 SQL」才 warn——两类失败日志待遇不一 | None content 也补一条 warn | safe |
| direct.rs:2975-2981 | 闸 1 语料 `meta.metric` 读取用 `.unwrap_or_default()` **静默吞错**——与 173-193 行 `reg_load!` 的立身之本（「读失败必须吼出来」）直接冲突；失败方向虽是更严（回落卡），但静默 | 改 `match` + `tracing::warn!` 后按空清单继续 | safe |
| direct.rs:3064-3065 | `unevidenced_joins > 0` 且 `join_pairs` 为空时，`join_evidence_edges` 的 PG 查询白跑一次——`derive_joins_unevidenced`（2892）第一句就返回固定文案，根本没用 edges | 先判 `shape.unevidenced_joins > 0` 直接 warn 回落，再取 edges | safe |
| direct.rs:3090-3094 | `sql.to_lowercase()` 写在 `filter` 闭包里——每张候选表都把整条 SQL 小写化一遍 | 循环外 `let sql_low = sql.to_lowercase()` 一次 | safe |
| direct.rs:3108-3113 | `customer_name_fragment` 只剥 `metric.name()`/`aliases()`，不剥 extras（1237-1245 的「销售金额/收入/毛利」）：「恒众本月销售金额」的 fragment 剩「恒众销售金额」→ 探库必空 → 漏接（安全方向，但与 1444/1565 的消化面不一致） | 同样剥 `sales_fact_metric_extra_words` | test |
| direct.rs:3147-3150 | 探库只判 `rows.is_empty()`，`LIMIT 3` 多取两行无用 | `LIMIT 1` | safe |
| direct.rs:3151-3152 | 探库的闸门失败/执行失败被 `.ok()?` 静默吞掉——DB 故障与「客户不存在」两种结局无法从日志区分（本文件 175-178 行事故笔记的正反对照） | 两个 `.ok()?` 改 `match`，失败补 `tracing::warn!` 后回落 | safe |
| direct.rs:153 vs 536 | `compose_verdict` 在 153 行算了一遍 `value_filters` 喂残留守卫，163 行 `compose_gated` 内部（536 行）又算一遍——诊断路径的重复计算 | 可接受（诊断低频）；如需省，compose_gated 收 vfs 形参 | safe |
| direct.rs:239-246 等 19 处 | `DirectHit { sql, route, prev: None, comparisons: vec![], detail: None, sales_context: None }` 五字段字面量在 239/894/1032/1473/1740/1798/1809/1823/1873/1963/1978/1990/2034/2049/2137/2243/2275/2376/3016 重复 19 次 | 本地 helper `fn hit(sql: String, route: &str) -> DirectHit`，变体处再覆盖字段 | safe |
| direct.rs:3260-3271,3480-3500,5293-5350 | 三处 `include_str!("direct.rs")` 自扫描断言用 `split("pub fn compose_hit").nth(1).expect(...)` 切函数边界——函数改名/顺序调整会让测试以难懂的方式红（刻意的接线钉，但缺统一 helper） | 抽 `fn body_between(src, start, end) -> &str` 测试工具，三处复用 | safe |

## KbPanel.vue（53 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| KbPanel.vue:1151 | `folderMoveTargets`（1162-1167）对每个候选行都调 `folderIsDescendant`，后者每次重建 byId Map，O(n²) | 在 computed 内建一次 Map，闭包复用 | safe |
| KbPanel.vue:315 | `dateText` 每次调用 new 一个 `Intl.DateTimeFormat`，文档行多时重复构造 | 模块级缓存一个 formatter 复用 | safe |
| KbPanel.vue:764 | 模板内联 `docs.filter((doc) => !doc.folder_id).length`（763 行「未分类」计数）每次渲染重算 | 提为 `unfiledCount` computed（与 `docs.some(...)` 的 v-if 共用） | safe |
| KbPanel.vue:764-770 | `counts` computed 对 docs 做 5 趟 filter | 单趟 reduce 聚合一次数组 | safe |
| KbPanel.vue:1256 | `send(files, retrying?)` 的 `retrying` 参数无任何调用方传入（1283 的「正在重新处理」文案从不出现），死代码 | 删除参数与分支文案，或补真正调用 | safe |
| KbPanel.vue:447-483 | `openMetadata` 不重置 `metadataSaving`：保存中文档 A 时打开文档 B，A 的 finally 因 requestId 失效跳过，`metadataSaving` 滞留 true，B 的保存按钮永久禁用 | `openMetadata` 开头置 `metadataSaving.value = false` | test |
| KbPanel.vue:1441-1458 | `generateDescription` 全程无 contextIsCurrent 守卫：成功后直接改写可能已不属当前空间的 `d.description`，失败也直接写 `actionErr`，与同文件其他动作（1474/1495）不一致 | 成功/失败路径都按 requestEpoch+requestSpace 守卫 | safe |
| KbPanel.vue:1410-1419 | `ingestUrl` 在 `uploadFolderId` 已失效（folders 过期）时静默落到根目录；而 `send()` 同情形明确报错「上传目标目录已失效」（1263-1266），同款两处口径不一 | 与 send() 对齐：requestedFolder 找不到 targetFolder 时报错返回 | test |
| KbPanel.vue:869 | `loadKnowledgeAssets` 在 docs 未内嵌 folders 时总调 `loadFolders`，即使上次已 404（`folderApiAvailable===false`），每次刷新多发一个注定失败的请求 | `folderApiAvailable === false` 时跳过 loadFolders | safe |
| KbPanel.vue:1768 | 「重试」目录调 `loadFolders()` 走 `++assetsRequestId`（832），会令正在进行的 docs 加载 requestId 失效被静默丢弃，列表停留在旧/空状态 | 重试时改调 `loadKnowledgeAssets()` 整体刷新 | test |
| KbPanel.vue:393 | `setTimeout(() => URL.revokeObjectURL(url), 0)` 0ms 回收在部分浏览器（Firefox）可能过早导致下载被截断 | 延迟到 1s 左右再 revoke | test |
| KbPanel.vue:893-930 | `loadSpaces` 自动换空间分支（preferred 失效等）不重置 `search`/`filter`/`actionErr`，而 `changeSpace`（984-987）重置——自动切换后旧关键词/筛选跨空间残留 | 两个分支对齐，补重置 | test |
| KbPanel.vue:1096-1097 | `createSpace` 失败写 `actionErr`，但新建空间对话框（2076 confirm-mask）仍开着并盖住正文错误条，用户看不到失败原因（文件夹对话框有专属 `folderDialogErr`，此处无） | 增设 dialog 内错误位或失败时关对话框 | test |
| KbPanel.vue:1207 | 删文件夹用原生 `window.confirm`，删文档用样式统一的 confirm-box（2303），同款确认两种形态 | 统一走自定义确认框 | safe |
| KbPanel.vue:1791 | 「删除」文件夹按钮只判 `folderDeletingId`，旁边「改名/移动」（1790）判 `switchingDisabled`；切换中删除仍可点 | 统一 `:disabled="switchingDisabled"` | safe |
| KbPanel.vue:1888 | 「重新加载」按钮无 `:disabled`，加载中可重复触发 | 加 `:disabled="loading"` | safe |
| KbPanel.vue:1902 | 「清除筛选」只清 search/filter，不清 `selectedFolderId`；在子文件夹里无匹配时点了仍空，文案却暗示能恢复列表 | 同时 `selectFolder('')` 或文案注明 | safe |
| KbPanel.vue:1839 | 「清除已完成」常亮（无已完成行时也可点），且实际连失败行一起清，文案只说「已完成」 | 无可清行时禁用；文案改「清除已结束」 | safe |
| KbPanel.vue:1869 | 搜索框 `type="search"` 自带原生清除 ×，又叠了自定义 × 按钮（1870），Chrome 下两个清除并存 | 改 `type="text"` 或去掉自定义按钮 | safe |
| KbPanel.vue:1869 | 占位文案「搜索名称、类型、状态或失败原因」漏了实际参与匹配的 tags/业务域/文档族/目录（788-789） | 文案补全或精简为「搜索文档」 | safe |
| KbPanel.vue:362-365 | `governanceText` 直接拼原始 `effective_from/to`（可能是完整 ISO），而列表 `effectiveText`（346-349）用 `dateInputValue` 截到日期，同款信息两处格式不一 | governanceText 也走 dateInputValue | safe |
| KbPanel.vue:302-309 | `typeText` 缺 XLSM/MARKDOWN/TIF/TIFF/LOG 及各图片格式映射（这些都在 47-51 的上传清单里），且 HTM 有映射但 accept 清单无 .htm，两份清单口径不一 | 补齐映射并对齐 accept | safe |
| KbPanel.vue:1277 | 超限文案硬编码「20MB」，未用 53 行的 `MAX_UPLOAD_BYTES`，常量改了文案就漂移 | 由常量推导文案 | safe |
| KbPanel.vue:1806 | 拖放高亮用 dragover/dragleave 直接置位，划过子元素（upload-mark 等）时 dragleave 频繁触发导致闪烁 | 用进入/离开计数或 relatedTarget 判定 | safe |
| KbPanel.vue:1325-1327 | `onDrop` 不识别拖入的文件夹，目录项会以 size 0 走 send() 报「文件为空，未上传」，误导 | 用 `webkitGetAsEntry()?.isDirectory` 提示「请改用上传文件夹」 | test |
| KbPanel.vue:1273-1315 | 全部文件预校验失败（无一发起请求）时，finally 仍 `loadSpaces` 整刷一次 | 记录是否发生过实际上传，没有则跳过刷新 | safe |
| KbPanel.vue:1820 | 「📁 上传文件夹」按钮在 `folderApiAvailable===false` 时不禁用，点击必走 1372-1390 的失败兜底 | 与 1732 的新建文件夹按钮同口径禁用并给 title | safe |
| KbPanel.vue:1944 | 「移动至」select 在目录接口不可用（folderApiAvailable===false）时不禁用，只剩根目录选项，操作多半失败 | 加 `folderApiAvailable === false` 禁用条件 | safe |
| KbPanel.vue:1691-1692 | 「共享权限」「新建空间」不受 `switchingDisabled` 约束，忙碌/切换中可开对话框叠加操作 | 加 `:disabled="switchingDisabled"` | safe |
| KbPanel.vue:565-567 | `switchingDisabled` 未纳入 `urlBusy`、`descGeneratingId`，URL 抓取/描述生成中可切空间（队列行随即被 903 清空，反馈丢失） | 补入这两个闸 | safe |
| KbPanel.vue:502-505 | 关联上限 50 硬编码两处（逻辑 502、文案 505），与 2272「最多 50 篇」共三处 | 提 `const MAX_RELATED = 50` 统一引用 | safe |
| KbPanel.vue:499-507 | `toggleRelatedDoc` 超上限写 `metadataErr` 后，用户删掉几篇再正常勾选时旧错误不清除 | 成功 toggle 时清 `metadataErr` | safe |
| KbPanel.vue:1933-1934 | `displayStatusText`（339-341）在 quality.label 存在时已返回它显示在 `<strong>`，同行 badge 又渲染一遍 `d.quality.label`，同一标签文本重复两次 | 二选一，或 strong 处改用 `docStatusText` | test |
| KbPanel.vue:2534 | `.doc-lineage:empty { display:none }` 特异性（0,2,0）低于 2583 的 `.doc-name-cell span:not(.file-type)`（0,2,1）且位置在前，永不生效；空 lineage 占位仍留 margin-top 缝隙 | 提特异性为 `.doc-name-cell .doc-lineage:empty` 或模板加 v-if | safe |
| KbPanel.vue:2735 | 注释「面板内容宽 < 文档表网格最小宽（≈800px）…820 之上先切卡片堆叠」与实际断点 1130px 不符（820 是下一个媒体查询） | 修正注释数字/描述 | safe |
| KbPanel.vue:1781 | 面包屑当前项只用 `.current` class，无语义标注（同文件文件夹节点用了 aria-current） | 补 `:aria-current="... ? 'page' : undefined"` | safe |
| KbPanel.vue:1877-1882 | 状态筛选按钮组无选中态语义（tablist 有 aria-selected，这里什么都没有） | 加 `:aria-pressed` | safe |
| KbPanel.vue:1697-1723 | role="tablist" 未实现方向键 ←/→ 在 tab 间切换（WAI  Tabs 模式），焦点管理不完整 | 加 keydown 左右键切换或去掉 tablist 语义 | test |
| KbPanel.vue:1664-1666 | 主对话框 aria-modal 但打开时不移焦、不圈禁焦点，Esc 之外的键盘路径可逃出到背景页 | 打开时聚焦标题/首控件，或补 focus trap | test |
| KbPanel.vue:2268 | 关联文档搜索占位「搜索文档名、文件夹、文档族或版本」漏了 tags（491 行匹配含 tags）；且该输入无 `type="search"`，角色搜索框（2131）有 | 文案补「标签」；统一 type | safe |
| KbPanel.vue:2245 | 「生效日期」无 `:max="metadataEffectiveTo"`，而失效日期已设 min（2249），单边约束 | 补 max 绑定 | safe |
| KbPanel.vue:2086 | 空间标识 pattern 校验失败时浏览器只给通用提示，无 title 说明合法字符 | 加 `title="仅字母、数字、下划线、短横线"` | safe |
| KbPanel.vue:1569-1570 | 授权部分失败只拼前 5 条失败详情，超过 5 条时无「等 N 条」提示，用户以为只有 5 条 | 末尾补 `等 N 条` | safe |
| KbPanel.vue:2262 | 「清空已选」在 `metadataSaving` 期间不禁用，保存中可改关联列表造成口径混乱 | 加 `:disabled="metadataSaving"` | safe |
| KbPanel.vue:110-1279 | uploads 队列只增不清（除手动「清除已完成」与切空间），批量上传大目录时行数无上限 | 设上限（如保留最近 200 条）截断 | safe |
| KbPanel.vue:1580-1585 | `saveGrant` 成功后手工再发一次 GET 刷新 grants/roles，与既有 `refreshGrants`（1502-1516）逻辑重复且漏刷 batch limit | 复用 refreshGrants（保留失败 codes 重选逻辑） | safe |
| KbPanel.vue:901-929 | 空间重置样板代码在 loadSpaces 成功（901-929）、changeSpace（984-1013）、loadSpaces catch（945-968）三处近乎逐行重复，已出现字段不一致（actionErr/search/filter 只在 changeSpace 清） | 提炼 `resetSpaceScopedState()` 一处维护 | safe |
| KbPanel.vue:322 | 命中位置用「目录 X」（空格分隔），文档行用「目录：X」（1914），同类信息分隔符不一 | 统一为「目录：」 | safe |
| KbPanel.vue:2732 | `.create-box input:focus` 规则与 2649-2652 的 `.create-box input` 分离在文件尾部，同组件样式两处维护 | 合并到主规则旁 | safe |
| KbPanel.vue:1169-1182 | `saveFolderEdit` 名称/父目录都没改时仍发 POST 并整刷 | 与 selectedFolder 原值比对，无变化直接关窗 | safe |
| KbPanel.vue:1821 | 按钮图标用 emoji「📁」，同面板其余图标为文本符号（⌂ ▰ ◇ › ⌄ ↑ ↻），风格不一且 emoji 渲染跨平台不一致 | 换文本符号统一 | safe |
| KbPanel.vue:2063-2073 | graph/mindmap/eval 三个面板用 v-if 链互斥挂载，切 tab 即销毁子组件（评估草稿、图谱缩放状态丢失），切回重来 | keep-alive 或文档注明预期 | test |
| KbPanel.vue:19 | `Doc`/`SearchHit` 同时有 `folder_path` 与 `directory_path` 双字段，无任何注释说明何者优先/为何并存（folderPath 实现里隐含的优先级只有读代码才知道） | 加一行注释说明 legacy 兼容与优先序 | safe |

## tools/embed_service.py（53 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| tools/embed_service.py:14 | 顶部 docstring 说 selftest「自造 md/csv」，实际已覆盖 json/html 四类（1295 行函数 docstring 才是新的） | 更新为「md/csv/json/html」 | safe |
| tools/embed_service.py:13 | docstring 里 /health 形状只写 `parse_caps:{...{ok,why}}`，未提 866 行已加的 `tiers` 机读两档 | 补上 `tiers:{text,ocr}` 键说明 | safe |
| tools/embed_service.py:17 | 用法串 `serve [port]` 漏了 host 位置参数（1690 行已支持） | 改为 `serve [port] [host]` | safe |
| tools/embed_service.py:26 | `sys.stdout.reconfigure` 在 pythonw（stdout=None）或 pytest 捕获流等无 `reconfigure` 的场景直接 AttributeError；stderr 未同样处理 | `getattr(sys.stdout, 'reconfigure', None) and ...`，stderr 同理 | safe |
| tools/embed_service.py:55 | 注释引用「桩睡 0.6s 实测 0.605s/0.002s」，但 1647 行桩已改 2s（1644-1646 自己解释了改因），顶部数字不再可复现 | 注明数字出自旧 0.6s 桩或更新表述 | safe |
| tools/embed_service.py:91 | `_H_MD` 要求 `#` 后必有空白，中文文档常见的 `#一级标题`（无空格）识别不出标题层级 | 放行 `#` 后无空格但排除 `#tag` 形态（如要求首字符非 ASCII 字母数字） | test |
| tools/embed_service.py:118 | `_read_text` 无 UTF-16 BOM 探测：Windows 导出的 UTF-16 txt/csv 会落入 gbk 解码成夹 NUL 的乱码而非正确解码 | 先试 BOM（`utf-16`），再走 utf-8-sig→gbk | test |
| tools/embed_service.py:126 | `_p_text`/`_p_html` 不过滤无词字符块（`.md` 里的 `---`/`***` 分隔线会成块进 chunk），而 `_p_pdf` 在 222 行有 `\w` 过滤——同款两口径 | 文本族入口复用同一过滤 | test |
| tools/embed_service.py:201 | 「三级降级」只在 import 失败时触发；`pymupdf4llm.to_markdown` 对损坏 PDF 抛运行期异常 → 整份 500，不降 fitz。docstring 的「降级」措辞易被读成含运行期 | 补一句说明，或捕运行期异常续降一级 | test |
| tools/embed_service.py:1249 | serve 先打印「服务就绪 :{port}」再于 1292 行才 bind；端口被占时日志先说就绪再抛 OSError，运维被误导 | 先构造 ThreadingHTTPServer 再打印 | safe |
| tools/embed_service.py:356 | `_pdf_fitz` 手工 `fitz.open`/`doc.close()` 无 try/finally（`get_text` 抛错则句柄泄漏），而 238 行 `_pdf_page_ocr` 用的是 `with fitz.open(...)`——同文件两写法 | 统一改 `with` | safe |
| tools/embed_service.py:373 | `_pdf_pypdf` 的 `PdfReader(path)` 从不 close，常驻 serve 下文件句柄泄漏 | 解析完 `r.close()`（pypdf≥3 支持） | safe |
| tools/embed_service.py:377 | `raise ParseError('no_text_layer')` 无 detail，而 332/336/382 同码错误都带人话 detail——界面只拿到裸错误码 | 补一句 detail（如「文本层为空」） | safe |
| tools/embed_service.py:241 | 函数内重复 import 顶层已有模块：241 `tempfile`、394 `re`、587 `io`、1641 `urllib.request`（顶层 24 行全已导入） | 删局部导入或统一风格 | safe |
| tools/embed_service.py:427 | docx 表格行用 `' | '` 连接且**无 ` | --- |
| tools/embed_service.py:439 | pptx 标题 shape 同时进 `heading_path` 和正文 `texts`（title 也有 text_frame），标题被向量化两遍 | `texts` 里排除 `slide.shapes.title` | test |
| tools/embed_service.py:445 | `_cell` 不 strip 单元格，xlsx 里 `' 10 '` 这类带空白值原样进 sheets/表头 | `_cell` 内 `str(v).strip()` | test |
| tools/embed_service.py:455 | `_sheet` 的 too_large 文案只有上限没有实际值（`列数超 200`），而 309 行 PDF 护栏带了实际页数——同款错误两口径 | 文案加 `实际 {len(r)}` / `实际 {len(keep)}` | safe |
| tools/embed_service.py:489 | `if s:` / 510 `if (s := ...)` / 523 `[s] if s else []` 恒真：`_sheet` 现契约恒返 dict（空表也返哨兵），是旧的返 None 契约留下的死判空 | 删死代码或注释说明保留意图 | safe |
| tools/embed_service.py:543 | docstring 称千问 key「或复用 `llm_api_key`」，但 583 行只读 `DMS_QWEN_OCR_KEY`/`QWEN_KEY` 环境变量，根本没有 settings 回退——注释许诺了不存在的行为 | 改注释，或真去接 settings 的 `llm_api_key` | safe |
| tools/embed_service.py:551 | 图片帧数无上限：多帧 tif/动图 gif 每帧一次 qwen HTTP（608 行超时 60s），N 帧最坏远超 Rust 120s 解析超时；PDF 侧有 `OCR_PAGE_CAP`（627）图片侧没有对应护栏 | 加帧数上限（复用 OCR_PAGE_CAP 口径），超了 too_large | test |
| tools/embed_service.py:556 | 多帧图片所有帧共用 `heading_path=文件名`，`chunk_blocks` 按 heading_path 分组合并时会加字符重叠，帧间 OCR 文本跨块重复 ~96 字符 | 帧 heading_path 带帧号（如 `name#f2`） | test |
| tools/embed_service.py:557 | `_p_image` 把 `_ocr_tesseract_frame` 抛的 ParseError 再包一层，detail 变成「OCR 失败（tesseract OCR 失败（lang=…）…）」双重套娃 | ParseError 原样 re-raise，只包非 ParseError | safe |
| tools/embed_service.py:618 | 618/620/622/627 四个 `int(os.environ.get(...))` 在 import 期执行，环境变量写错（如 `DMS_OCR_DPI=abc`）→ 裸 ValueError traceback，服务起不来且不知所云 | 包一层友好报错（指出是哪个变量、值是什么） | safe |
| tools/embed_service.py:629 | 注释「首次转换含 LibreOffice 建 profile，慢」有误导：`_p_legacy` 每次调用都用全新临时 profile（643-644），**每次**转换都付建 profile 成本，不止首次 | 修正注释表述 | safe |
| tools/embed_service.py:716 | qwen key 已配但 PIL 缺失时，报错建议 `pip install pillow pytesseract`——qwen 路根本不需要 pytesseract，误导运维多装 | 按缺失路径分别给安装建议 | safe |
| tools/embed_service.py:748 | CAPS 有 `.html` 无 `.htm`，直接上传 `page.htm` 报 unsupported（MIME 表 767 行也救不了无 mime 的调用） | CAPS 补 `.htm` → `_p_html` | test |
| tools/embed_service.py:773 | mime 匹配未 lower：`Application/PDF` 按 RFC 合法但匹配不上 MIME_EXT，静默落到扩展名 | `.strip().lower()` 后查表 | test |
| tools/embed_service.py:776 | `ParseError('unsupported', ext or mime or path)` 的 detail 只是裸 `.rar`/路径，对比 783 行 `{ext} 暂不可用：{why}` 的人话，同款错误两口径 | detail 改「不支持的格式：{ext}…」 | safe |
| tools/embed_service.py:808 | `_EXE_CACHE` 只按 `env` 名做键：同名 env 不同 `names` 的两个调用点会互相污染缓存（当前无此调用，纯埋雷） | 键改 `(env, names)` | safe |
| tools/embed_service.py:898 | `_emit` 用 `assert` 守 MAX_TOKENS 运行时不变量，`python -O` 下整条被剥掉 → 超窗块静默进库被 fastembed 截断 | 改显式 `raise ValueError` | test |
| tools/embed_service.py:973 | `hard_cap` 恒 ≥ `budget`（target≤480 ⇒ tc≤768=`int(480*1.6)`），`min(budget, hard_cap)` 永远取 budget——死防御代码且无注释 | 删 hard_cap 或注释说明纯防御意图 | safe |
| tools/embed_service.py:1001 | 阈值魔法数 50（`tc - len(prefix) - 1 < 50` 回通用路径）只注释了宽表情形，没说 50 怎么来的 | 注释补一句依据 | safe |
| tools/embed_service.py:1026 | `_revec` 未校验 embed 返回条数与 rows 等长：zip 静默截断、少写的行照旧计入返回数（Rust 侧 embed.rs:367 正是「少返一行→整批 None」的纪律，python 侧没有）；1183 行 revec_chunks 同构 | 比较长度，不等则该批按失败处理 | test |
| tools/embed_service.py:1032 | `build` 的 `psycopg2.connect` 无 connect_timeout，而 `revec`（1213）有 5s——同款连接两口径 | build 也加 `connect_timeout=5` | safe |
| tools/embed_service.py:1032 | `build` 的 `pg.close()`（1071）无 try/finally，中途异常连接残留；`revec`（1223-1224）有 finally——两口径 | build 补 try/finally | safe |
| tools/embed_service.py:1033 | `build` 全程无 statement_timeout/lock_timeout（`revec` 1220 行有且写了理由）；HNSW 索引重建卡在锁上会无声挂死 | build 复用同一组 SET | test |
| tools/embed_service.py:1037 | table_doc 的 SELECT 无 `embedding IS NULL` 过滤（exemplar 1048、element 1064 都有），每次 build 全量重算所有表向量，现场无注释说明是否刻意 | 注释说明取舍，或补 IS NULL 增量 | safe |
| tools/embed_service.py:1041 | `DROP INDEX IF EXISTS` 后接 `CREATE INDEX IF NOT EXISTS`——drop 之后 IF NOT EXISTS 恒真，组合读起来像不确定是否 drop 过（1067-1069 同样） | 改裸 `CREATE INDEX` | safe |
| tools/embed_service.py:1079 | 注释「Rust `upsert_datasource` 在 description 变更时置 NULL 作失效」 overstated：Rust 还有第二条 upsert 路径（registry/datasource.rs:181-183）改 description 却**不清 embedding**，陈旧向量仍可能存在 | 注释补限定；真正的修复在 Rust 侧那条 upsert | safe |
| tools/embed_service.py:1115 | KB_SEL 只捞 `embedding_recipe=KB_RECIPE` 的 NULL 行，KB_MISS（1128）却把 `recipe<>KB_RECIPE` 的行也算「仍缺」——若存在旧 recipe 的 NULL 行，revec 永远修不完且恒退 1，运维无提示 | KB_SEL 去掉 recipe 过滤（捞全部 NULL），或 MISS 口径对齐 | test |
| tools/embed_service.py:1189 | `_promote` 只判本次补过的 docs；历史遗留「块齐但 status 卡 chunked」的文档（崩溃窗口造成）永远不被推进，KB_MISS 也计不到它 → revec 退 0 而文档仍检索不到 | KB_DOCS 的判定扩到全量 chunked 文档，或注释说明只管本批 | test |
| tools/embed_service.py:1220 | 两条 SET 塞在一个 execute 里靠 psycopg2 简单查询协议生效，读者容易误以为会报错 | 拆成两次 execute | safe |
| tools/embed_service.py:1232 | `path.startswith('/parse')`/`'/chunk'` 把 `/parseXYZ`、`/chunky` 也路由进对应 handler，与 1231 行「未知路径按 /embed」的兼容口径互相打架 | 先 `path.split('?')[0]` 再精确等值匹配 | safe |
| tools/embed_service.py:1233 | 三个端点均无请求体类型校验：`/parse` 的 path 传 list → splitext TypeError 500；`/chunk` 的 blocks 传 dict/str → b.get AttributeError 500；`/embed` 的 texts 传 `"abc"` 会按字符逐一向量化（静默错），传 `[1]` → 500；texts 条数也无上限可长占 `_EMBED_LOCK` | 校验类型+条数上限，非法给 400/422 | test |
| tools/embed_service.py:1237 | **bug**：`int(body.get('overlap') or OVERLAP)` 把显式 `overlap: 0` 吞成默认 60——HTTP 客户端无法关闭重叠（selftest 里 overlap=0 只走了直接调用，没走 HTTP）；1236 行 `target_tokens` 同模式，且 `'abc'` 这类值 → ValueError 500 | 改 `body.get('overlap', OVERLAP)` + None 判断 | test |
| tools/embed_service.py:1239 | `/embed` 缺省 `query=true`：调用方漏传就静默进 query 向量空间（与 passages 不同空间，检索恒排后），错的方向是静默的 | 缺省改 false 或必填（需评估 Rust 客户端兼容性） | test |
| tools/embed_service.py:1255 | `self.path == '/health'` 精确匹配，`GET /health?ts=…`（常见探活防缓存写法）→ 404 | 比较 `self.path.split('?')[0]` | safe |
| tools/embed_service.py:1268 | `int(self.headers.get('Content-Length', 0))` 在 try 外：畸形 Content-Length → ValueError 未被捕获 → 连接被掐断无任何响应 | 挪进 try，按 400/500 回 | safe |
| tools/embed_service.py:1270 | 请求体无大小上限（大 Content-Length 直接全读进内存），且非法 JSON 走 `except Exception` 回 500 而非 400 | 加体积上限（413）+ JSONDecodeError 单列 400 | test |
| tools/embed_service.py:1277 | 500 把 `str(e)` 原样回客户端，可能带服务器绝对路径/内部细节；serve 支持绑非回环地址（1690）时构成信息外泄 | 通用文案 + 细节打服务端日志 | test |
| tools/embed_service.py:1299 | selftest 用 `tempfile.mkdtemp()` 从不清理，每跑一次留一个临时目录 | 改 `TemporaryDirectory()` 上下文或末尾 rmtree | safe |
| tools/embed_service.py:1659 | `_selftest_serve_unblocked` 起的 serve 无法关闭（serve 不返回 server 对象），靠 daemon 线程+进程退出兜底，现场无注释说明 | serve 返回 server 或注释说明兜底口径 | safe |

## DataMapPanel.vue（46 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| DataMapPanel.vue:5 | 头注释称 kind 只有 5 值，代码 L33-34 与后端闭集（datamap_api.rs:40）都是 7 值，join/lineage 未提 | 注释补全 7 值 | safe |
| DataMapPanel.vue:10 vs 739 | 注释写了「空白平移」，面板副文案只说「拖拽、滚轮缩放」，用户不知道空白可拖 | 副文案补「空白拖动平移」 | safe |
| DataMapPanel.vue:34 | `join #2f6fd0` 与 `joinable #4a90d9`、`lineage #7a5af5` 与 `synonym #9b6de8` 色差过小，图例肉眼难分 | 拉开色相 | safe |
| DataMapPanel.vue:37 | `join='合同关联'` 与 `joinable='可关联'` 仅一字之差，并排图例易混 | 改「已注册关联」类更区分度文案 | safe |
| DataMapPanel.vue:42 | legendKinds 固定 7 项，当前数据里没出现的 kind 也占图例 | 按 edges 实际 kind 集合过滤 | safe |
| DataMapPanel.vue:98-99 | 空 kind 节点按 index 取色：同 kind('') 节点颜色不一且随加载顺序漂移 | 空 kind 给固定色 | safe |
| DataMapPanel.vue:135 vs 196 | `r:10` 初始值必被 finishGraph（min≈12.4）覆盖，是死值 | 删除或对齐注释 | safe |
| DataMapPanel.vue:140 | 头注释边契约写 `source\ | from / target\ | to`，与后端实际 `left_table/right_table`（`source` 是来源标识）不符 |
| DataMapPanel.vue:150,159-166 | indexById 只按 id 建索引；后端节点 id 是 `table:t_a`、边端点是裸表名 → ensure() 必造重复占位节点；runPath 的 byName(L635-639) 已按 id+label 双键，两处口径不一 | ensure 索引同时按 label/裸表名建 | test |
| DataMapPanel.vue:172-174 | **边端点归一缺 `left_table`/`right_table`**：后端边 JSON 的 `source` 是 'inferred'/'registry'（datamap_api.rs:607/657），被误当端点，dst='' → L174 把每条边都丢弃，画布永远没有边 | 链首加 `row.left_table`/`row.right_table`（须在 `row.source` 之前） | test |
| DataMapPanel.vue:176 + 837-847 | registry 边 `id:null` → 合成 id `e${index}`，admin 详情卡仍对其渲染接受/拒绝 → POST 必 404「推断边不存在」 | 非数值/合成 id 不渲染操作区 | safe |
| DataMapPanel.vue:107-109 | normStatus 不认后端 registry 边的 `active` → 落 'pending'，合同边被画成「待确认」虚线 | 映射 `'active'→'accepted'` | test |
| DataMapPanel.vue:196 | `sqrt(max(1,degree))` 使 0 度与 1 度节点同半径，度数信息丢一档 | 用 `max(0,...)` 或调整基线 | safe |
| DataMapPanel.vue:272 + 803-806 | 无 pointercancel/pointerleave 监听：触摸被打断时 drag.mode 滞留 'node'，RAF 因 L272 条件永远不停；hover 高亮/cursor 也滞留 | 补 pointercancel/pointerleave 收尾 | safe |
| DataMapPanel.vue:356 | 主题色 render 时读 `dataset.theme`，但主题切换不触发 render，模拟休眠后画布配色滞留到下次交互 | 监听 theme 变化或 wake 时补 render | safe |
| DataMapPanel.vue:379,409 | 边/节点标签 `slice(0,10/12)` 静默截断不加省略号 | 截断时补「…」 | safe |
| DataMapPanel.vue:415-427 | pointerdown 把节点中心瞬移到指针（无抓取偏移），点大节点（r 可达 24）边缘会跳 | 记录并保留抓取偏移 | safe |
| DataMapPanel.vue:438,448 | 任意 1px 位移即 `moved=true`，点击手抖就变成拖拽/反选 | 加 ~4px 位移阈值 | safe |
| DataMapPanel.vue:454-457,674 | load() 清 hoverNodeId 但不复位 canvas cursor，旧 'pointer' 光标滞留 | 一并复位 cursor | safe |
| DataMapPanel.vue:479-489 | onWheel 未归一 `deltaMode`（Firefox 行滚动缩放暴涨）；且无任何「复位视图」入口（zoom/pan 只有重开面板才复位 L701-703） | 归一 deltaMode；加复位按钮 | safe |
| DataMapPanel.vue:504-508,824 | selectedEdgeNodes 不校验 a/b 是否存在，模板直接 `.label`，索引异常时运行时报错 | 守卫 a/b 缺失返回 null | safe |
| DataMapPanel.vue:514 | selectedNodeEdges 截 8 条但无「还有 N 条」提示，信息隐藏 | 追加计数提示 | safe |
| DataMapPanel.vue:539-540 | 搜索按原始 kind（'co_occurs'）匹配，UI 显示的是中文标签 → 输「共现」搜不到 | 连同 kindLabel 一起匹配 | safe |
| DataMapPanel.vue:570,613 | decide 成功 note 永不自动消失，runPath 只清 error 不清 note → 旧绿条与路径结果长期并存 | note 加超时或选择/查询变化时清 | safe |
| DataMapPanel.vue:584-598 | **extractPaths 优先取 `root.path`**；后端 `path` 是 hop 对象数组（left_table/right_table/card/forward，datamap_api.rs:695-717），asRef 找不到 id/name/table → 全被滤掉 → found=true 也报「没有找到路径」 | 优先用 `nodes`（裸名数组）或解析 hop 的 left/right_table | test |
| DataMapPanel.vue:643-645 | ids filter(Boolean) 后再配对你邻，中间节点未解析时把不相邻两点连成假路径高亮 | 有未解析节点就断段，不跨段配对 | safe |
| DataMapPanel.vue:695-696 | 只认 `{nodes}`/`{edges}` 包裹键；SqlAuditPanel normalize 认 items/rows/records/entries，与 L11「宽容归一」自述口径不一 | 对齐多包裹键或收窄注释 | safe |
| DataMapPanel.vue:704-706 | `alpha=1` 且排了 RAF 后又立即 render()，首帧画两次 | 去掉其中一次 | safe |
| DataMapPanel.vue:724-728 | 卸载不 abort 在途 fetch（load/decide/runPath），回调仍写已卸载组件的 ref 并 emit | AbortController 统一收尾 | safe |
| DataMapPanel.vue:733 | dialog 无初始焦点/焦点约束，Tab 可跑出弹窗 | 挂载时聚焦关闭按钮 | safe |
| DataMapPanel.vue:742,825,854 | 三个关闭按钮内容 ✕/× 充当可访问名（title 不保证成为 accName），且两字形同款功能不一 | 统一字符并加 aria-label | safe |
| DataMapPanel.vue:747,749 | `@keyup.enter` 在中文 IME 回车选词时误触发路径查询 | 改 `@keydown.enter` 并判 `!isComposing` | safe |
| DataMapPanel.vue:751 | datalist 以 label 为 value，列节点 label 与表名重复（见下条）→ 候选列表大量重复项 | 候选去重 | safe |
| DataMapPanel.vue:753-755 | 查路径未挡 `from===to`，恒等查询浪费且高亮无意义 | disabled 或内联提示 | safe |
| DataMapPanel.vue:757,760,761 | pathMsg/error/note 均无 role="status"/"alert"（L807 loading 却有 role=status，同文件不一） | 补齐 role | safe |
| DataMapPanel.vue:773,786 | 空态「暂无关系/暂无表节点」不区分「真没有」与「搜索过滤光了」 | needle 非空时显示「无匹配结果」 | safe |
| DataMapPanel.vue:794 | 「{{ n.degree }} 边」黑话 | 改「条关系」 | safe |
| DataMapPanel.vue:814 vs 767 | dm-count 的「关系」=canvasEdges.length（不含 rejected），tab 计数=edges.length（含 rejected），同名不同数 | 注明口径或统一 | safe |
| DataMapPanel.vue:815-819 | 图例恒渲染，DOM 序在 dm-state 之后 → 加载/空态时浮在遮罩层上方 | `v-if="nodes.length"` | safe |
| DataMapPanel.vue:841 | 拒绝按钮无「提交中…」反馈（L839 接受、L845 撤销接受都有），同款不一 | 补 busy 文案 | safe |
| DataMapPanel.vue:857 | 节点 kind 原文 'table'/'column' 直接展示（边有中文映射）；后端已给的 domain/row_estimate 也未利用 | 加映射，可选展示 domain/行数估计 | safe |
| DataMapPanel.vue:889-890 | .dm-path 无 flex-wrap，窄屏输入框 `24vw` 会溢出 | 加 wrap 或媒体查询 | safe |
| DataMapPanel.vue:930-931 | dm-spin 无限动画未尊重 prefers-reduced-motion | media query 停动画 | safe |
| DataMapPanel.vue:323-331 | nodeDimmed 每节点每次 render 都 `canvasEdges.some(...)` → hover 帧 O(N·E) | hover 变化时预计算邻接 Set | safe |
| DataMapPanel.vue:72-87 | authQuery/authTail/errText 与 SqlAuditPanel L35-47、SkillsPanel、UsagePanel 逐字重复 | 抽共享 fetch 工具 | safe |
| DataMapPanel.vue:153 | 节点 label 链缺 `row.column`：列节点（id `column:t.c`）全部以裸表名为 label，同表列节点无法区分 | label 加 `column` 维度（`t.c`） | safe |

## crates/connector/src/mysql.rs（46 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/connector/src/mysql.rs:1142 | `to_table` 在 push 后才判 `data.len() >= max`，`max == 0` 时仍返回 1 行；而 `DsPolicy::max_rows: Some(0)` 的契约是「恒空结果」(source.rs:71)，`effective_limits` 不会拦 0 —— 真实 off-by-one。postgres.rs:306 同形 | 改为 push 前判满，或 `rows.iter().take(max)`；补 max=0 单测 | test |
| crates/connector/src/mysql.rs:1163 | `cell_kind` 漏 `MEDIUMINT UNSIGNED`（sqlx 类型名表里有；其余四个 UNSIGNED 整型都枚举了）→ 落 `Cell::Text` → `try_get::<String>` 失败 → 该列静默全 Null | Int 臂补 `"MEDIUMINT UNSIGNED"`，单测补一行 | test |
| crates/connector/src/mysql.rs:1183-1193 | `Cell::Time` 只尝试 `NaiveDateTime`/`NaiveDate`；MySQL `TIME` 型 sqlx 解为 `NaiveTime`，两次尝试均失败 → `TIME` 列静默全 Null（1167 行把 TIME 映射进 Time 族） | 追加 `NaiveTime` 分支（`%H:%M:%S`）；带库验证或至少注释标注 | test |
| crates/connector/src/mysql.rs:1112-1125 | `describe_columns` 无超时：`pool.acquire()` + `conn.describe()` 都在 `fetch` 的 deadline 之外，数仓链路挂住时 fetch 整体失去上限 | 用剩余 deadline 或小常量 timeout 包住整段 | test |
| crates/connector/src/mysql.rs:1118 | `pool.acquire()` 失败直接 Err 让整个 fetch 失败；而同函数 describe 失败只降级空列（1121-1124）—— 同一回填步骤两种失败口径不一致 | acquire 失败同样 warn + 返回空列 | test |
| crates/connector/src/mysql.rs:680-684 | 数仓只读核验用 `DMS_LOOKUP_TIMEOUT`(2s)，与本文件 37-39 行自述的公网链路现实（单条探针 ~27s）矛盾 → 公网数仓 `swap_pool`/`test_pool` 必按非只读拒掉 | 数仓路径给独立预算（复用 `WAREHOUSE_CATALOG_TIMEOUT` 量级） | test |
| crates/connector/src/mysql.rs:505-510 | `test_pool` 的 `VERSION()` 也用 2s —— 与上一条同款公网链路问题，数仓「测试连通性」按钮会间歇超时 | 数仓能力放宽该超时 | test |
| crates/connector/src/mysql.rs:168 | 生产能力下 `connect_read_only` 内部已验 `mysql_session_read_only`(425 行），`swap_pool_named` 又 `pool_read_only` 全量重验 —— 生产路径多一次重复核验 RTT | 第二次核验只在 `capability.is_warehouse()` 时做 | test |
| crates/connector/src/mysql.rs:212-232 | `fetch_dms_lookup` 不套 `ds_policy`（对比 `fetch` 744 行）—— 同源两条取数通道 A8 策略口径不一致，点查无法被 ds 级策略进一步收紧 | 复用 `effective_limits(false, self.ds_policy(), …)` 取 min | test |
| crates/connector/src/mysql.rs:767-801 | `explain` 同样不套 `ds_policy` 超时收紧，`fetch` 有 A8 clamp —— 同源不一致 | 与 fetch 同款 `policy.clamp` | test |
| crates/connector/src/mysql.rs:219-220 | `acquire_lookup_slot` 超时报错文案用满额 `DMS_LOOKUP_TIMEOUT`，实际只等了 deadline 剩余量 —— 「等待 2.0s 未返回」误导（749 行 fetch 同款） | 传入 `deadline.saturating_duration_since(now)` | test |
| crates/connector/src/mysql.rs:254-257 | 拒绝文案硬编码 "production_lookup 禁止…"，实际拒绝时能力可能是 `IdentityPermission` —— 文案与事实不符；274-277、322-325、364-367、808-811 同病 | 文案插值 capability 名 | test |
| crates/connector/src/mysql.rs:647-650 | 空 `lookup_cols` 与「索引核验不过」两种拒绝共用一条文案（620 行空集直接短路到同一 Err），排障时分不出是哪类 | 空键单独文案 | test |
| crates/connector/src/mysql.rs:511 | `VERSION()` 失败被 `connection_unavailable` 吞掉原始 sqlx 错误，丢诊断（同文件其他路径用 `sqlx_err` 保留 Database 消息） | 改 `sqlx_err` 或 warn 留痕原始错误 | test |
| crates/connector/src/mysql.rs:536-537 | 表名不合法报错不带是哪个表 | 文案带 `{table}` | test |
| crates/connector/src/mysql.rs:499-503 | `test_pool` 只读核验失败早退时未 `pool.close().await`（对比 426 行同场景显式关闭）—— 一次性池虽随 Drop 收尾，两处口径不一 | 早退前 `pool.close().await` | safe |
| crates/connector/src/mysql.rs:424 | 建池失败 `.map_err(\ | _\ | …)` 吞掉底层错误（认证失败 vs 网络不可达无法区分） |
| crates/connector/src/mysql.rs:242 | 信号量是进程级 `OnceLock` 且从不 `close` → `AcquireError` 分支（"并发阀门已关闭"）不可达死代码 | 删除该 map_err 或注释标注防御意图 | safe |
| crates/connector/src/mysql.rs:41-44 | 信号量进程全局：若未来多生产源共存会共享 2 槽互相饿死，注释只覆盖单库语义 | 补一行注释说明全局语义是有意的 | safe |
| crates/connector/src/mysql.rs:789-790 | `explain` 中 `warehouse` 恒 true（775 行已早退非数仓），`t.min(DMS_LOOKUP_TIMEOUT)` 是不可达死分支；791 行注释「生产 MySQL 到这里已经过闸门」与早退矛盾 | 删死分支直接用 `t`，修注释 | safe |
| crates/connector/src/mysql.rs:35 | 注释「逐表 SHOW INDEX」与实现不符 —— 实际查 `information_schema.STATISTICS`（539-542 行自己解释了为何不用 SHOW INDEX）；531 行同病 | 注释改为 information_schema | safe |
| crates/connector/src/mysql.rs:1129 | `fn to_table(...) {    let mut columns…` 签名与首条语句挤一行 —— rustfmt 会拆开，全仓 fmt 基线上的毛刺 | 拆行 | safe |
| crates/connector/src/mysql.rs:543-545 | SQL 字面量内含超长空格串（多行拼接被压成单行的残留），可读性差 | 用续行 `\` 或 `concat!` 恢复 | safe |
| crates/connector/src/mysql.rs:523-530 | `registered_lookup_keys()` 调两遍（一遍建 map、一遍集 tables） | 单次迭代同时收两份 | safe |
| crates/connector/src/mysql.rs:568 | `visible` 恒 `true`，586 行 `!*visible` 恒假 —— 死字段死分支（541-542 注释解释了语义来源但代码仍可简化） | 删字段，注释保留「不查 IS_VISIBLE」的决策 | safe |
| crates/connector/src/mysql.rs:578-583 | `sort_by_key` 后只用 `.first()` 和 `len()`；且 `else { continue }` 不可达（`or_default` 保证非空） | `min_by_key` 免排序，删不可达分支 | safe |
| crates/connector/src/mysql.rs:587 | `index_type.to_ascii_uppercase()` 每索引一次堆分配 | `eq_ignore_ascii_case("BTREE") \ | \ |
| crates/connector/src/mysql.rs:591 | `table.to_ascii_lowercase()` 在列循环内重复计算 | hoist 到表循环外 | safe |
| crates/connector/src/mysql.rs:622-626 | `ensure_verified_lookup` 循环内每列重复 `sql.table().to_ascii_lowercase()` + `registered_lookup_kind` 线性扫静态表 | 表名小写 hoist；registered 结果复用 523 行的 map | safe |
| crates/connector/src/mysql.rs:293-311 | `decoded.0..decoded.7` 数字下标拆 8 元组，可读性差 | `let (…, ordinal_text) = decoded;` 具名解构 | safe |
| crates/connector/src/mysql.rs:343 | `to_lowercase()`（Unicode 全折叠）与本文件其余 `to_ascii_lowercase()`（525/591 等）不一致；标识符本就 ASCII；1475 行同病 | 统一 `to_ascii_lowercase` | safe |
| crates/connector/src/mysql.rs:1475 | `fill_blank_comments` 每列一次 `(table, col)` 双 String 分配做查找 | 预小写化或换迭代方向 | safe |
| crates/connector/src/mysql.rs:975 | `for (_, mut table) in found` 弃键迭代 | `found.into_values()` | safe |
| crates/connector/src/mysql.rs:955 | 列去重 `iter().any` O(n²)（列数小，低优先） | HashSet 或保留并注明规模假设 | safe |
| crates/connector/src/mysql.rs:373-380 | `ping` 用 `fetch_all` 收 `SELECT 1` | `fetch_one`/`fetch_scalar` 更省 | safe |
| crates/connector/src/mysql.rs:403 | 建池 info 日志缺 `ds`/`max_conn` 字段；warehouse 分支也打「创建只读业务连接池」，文案说"业务"不准确 | 加结构化字段，文案按能力区分 | safe |
| crates/connector/src/mysql.rs:1099 | `redact` 的 warn 无 `reason`/源标识字段，与 438/667/688/692/1122 的结构化 warn 风格不一 | 加 `reason = "sensitive_columns_redacted"` 和 `source` 字段 | safe |
| crates/connector/src/mysql.rs:336-339 | `Err(_)` 吞掉映射表不可用的具体错误，info 日志无 `err` 字段（「静默跳过」是意图，但诊断全丢） | 日志加 `err = %e` | safe |
| crates/connector/src/mysql.rs:212-232 | 生产点查无慢查询 warn（fixed.rs 有 500ms 阈值慢日志）—— 1.9s 慢点查无观测；`fetch` 755 行同 | 加同款 slow-log（只加日志） | safe |
| crates/connector/src/mysql.rs:128 | `.expect("锁中毒")` panic 路径（141/189/195/714/722 同）：PoolState 是整结构替换无部分态，`into_inner` 恢复是安全的 | `unwrap_or_else(PoisonError::into_inner)` | safe |
| crates/connector/src/mysql.rs:846-861 | `probe_mysql_schema` 两条探针无超时（286 行目录探针有 60s）—— 公网链路挂住要等到 TCP 层 | 包 `WAREHOUSE_CATALOG_TIMEOUT` 量级超时 | test |
| crates/connector/src/mysql.rs:259 | `raw_all` 无超时（`enrich_dms_snapshot` 走它）；369 行 `raw_dates_all` 同 —— 数仓静态查询整体无上限 | 包超时 | test |
| crates/connector/src/mysql.rs:385-388 | `/api/health`（server/main.rs:2271）每次健康检查都对数仓实跑三级权限 UNION 查询，公网 RTT 级开销且结果短时不变 | 短 TTL 缓存（如 30s） | test |
| crates/connector/src/mysql.rs:493-517 | `test_pool` 对生产能力会跑完整索引核验（30s 预算）才返回，页面「测试」按钮延迟远超直觉；返回值注释未说明延迟含核验 | doc 注明，或测试路径跳过索引核验 | test |
| crates/connector/src/mysql.rs:478 | `split_once('?')?` 在 472 行计数校验后永不为 None —— 不可达的 None 分支让读者误判失败模式 | 重构为 `expect` 或 zip 迭代 | safe |
| crates/connector/src/mysql.rs:472 | `template.matches('?')` 计数包含模板字符串字面量内的 `?`，doc 未写明该约束 | 补一行 doc | safe |

## main.rs（44 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| main.rs:1 | 文件头注释停在「M0 骨架（/api/health）+ M1 权限内核」，与现状（约 90 条路由、十几个 CLI 子命令）严重不符 | 更新文件头为一行现状概述 | safe |
| main.rs:30-32 | 「路由注册不在本轮…集成方接线后这个 allow 一行删掉」：usage_api 早已接线（1336 行），且根本没有 allow 行 | 删除这段过期注释 | safe |
| main.rs:131 | `self.cfg.read().expect("cfg 锁中毒")`：一次锁中毒后所有 handler 永久 panic | `unwrap_or_else(\ | e\ |
| main.rs:261-263 | `.map_err(\ | _\ | anyhow!("分析库目标 {target} 连接失败…"))` 吞掉底层错误（DNS/认证/超时原因全丢），启动排障只能猜 |
| main.rs:284-286 | `auth_source` 同样的 `\ | _\ | ` 吞错 |
| main.rs:314-316 | `owned_store` 同样的 `\ | _\ | ` 吞错 |
| main.rs:241-244 | 配置无效用 `panic!`，而 `main` 返回 `anyhow::Result`——启动失败路径两种风格混用，panic 无上下文链 | 改为返回 Err（`llm_client` 改签名或调用点 `unwrap_or_else` 换 `?`） | test |
| main.rs:651 | `datamap-calibrate` 的 days：`s.parse().ok().unwrap_or(30)`——用户敲 `abc` 静默按 30 天跑，正是本文件 parse_why_args 反对的「宽容解析」 | parse 失败 `bail!`；顺带拒绝 0 | test |
| main.rs:672 | `lineage-build` 输出 `println!("{r:?}")` Debug 格式，其他子命令全是 JSON stdout，脚本无法解析 | 改 `serde_json::json!` 输出 | test |
| main.rs:886 | `audit-exemplars` 用 `args.iter().any(\ | a\ | a == "--fix")` 扫全部 argv（含程序名），且未知 flag 静默忽略，与 parse_why_args 的严格哲学自相矛盾 |
| main.rs:900 | `build_rules(...).await.unwrap_or_default()`：PG 出错→空规则→`check_caliber` 恒干净→审计假绿，恰是该子命令存在要防的失败形状 | 失败时 `tracing::warn!` 并计入统计（不静默当零违规） | safe |
| main.rs:992 | `stdin.lock().lines()` 在 `#[tokio::main]` 里同步阻塞读 stdin，题间阻塞占住一个 runtime worker | 换 `tokio::io::stdin()` + `BufReader::lines()` 或 `spawn_blocking` | test |
| main.rs:1027-1035 | stdin 读取出错只回一行 error 仍继续循环；持续性 IO 错误会无限刷错误行刷满下游 | Err 分支输出后 `break` | test |
| main.rs:1098 | `exec-sql` 的 role 位 `args.get(4).map(...)` 不过滤空串，而 `ask`（1058）专门用 `slot()` 过滤并写了为什么；`exec-sql u "sql" ""` 会去查空角色 | 两处共用同一空串过滤 | test |
| main.rs:1125 | `scope` 子命令同样不过滤空 role | 同上 | test |
| main.rs:1217/1221 | `graph_status` 连续两次 `lock().unwrap()` 取同一值；且这里 unwrap 与 health（2329）的容错式 lock 风格不一 | 单次 lock；统一为中毒容错写法 | safe |
| main.rs:1192/1308 | `cfg.kb_max_mb * 1024 * 1024` 同一计算写两遍（第二处还带 `as usize` cast），改一处忘另一处 | 提一个小函数/常量共用 | safe |
| main.rs:1413 | warn 文案里塞大段空格做对齐（`"login_name              ——"`），进结构化日志/采集后非常难看 | 去掉对齐空格，用字段承载 | safe |
| main.rs:1416-1417 | 先 `info!("listening on ...")` 再 bind；bind 失败时日志已谎称在监听 | bind 成功后再打日志 | safe |
| main.rs:1418 | `axum::serve(...).await?` 无 graceful shutdown：Ctrl-C 直接掐断在途 ask 和 spawn 出的观测 INSERT（CLI 分支 1085 行都专门 await 它） | 加 `with_graceful_shutdown` 监听 ctrl-c | test |
| main.rs:1466/1472/1475 | SSO 链全部 `.map_err(\ | _\ | ...)` 吞底层错误且不留 warn，线上「读取 DMS 角色失败」查不到原因 |
| main.rs:1634/1641/1655 | api_login 同样三处 `\ | _\ | ` 吞错无日志 |
| main.rs:1586-1590 / 1649-1653 | 「单角色自动选、零角色 admin 兜底」逻辑在 wework/login 两处复制，SSO 走 `sso_role` 函数、api_roles（2070）又写第四遍 | 收敛为一个共享函数 | safe |
| main.rs:1626-1628 | 密码 >256 字节时报「请输入账号和密码」，用户明明输了，文案误导 | 超长单独报错文案 | safe |
| main.rs:1730-1731 | resolve_identity 头上两行注释互相矛盾：「回退 login_name」 vs 「Bearer 会话 token 是唯一可信来源」 | 删第一行旧注释 | safe |
| main.rs:1749 | 每请求 `st.cfg()` 整份克隆 Settings 只为读 `mcp_keys`；且与 mcp_api 用的静态 `st.mcp_keys`（109 行）是两份数据——热更新 key 后 X-API-Key 通道与 /api/mcp 口径漂移 | 统一两处读取来源（热更新以 cfg 为准则 mcp_api 也走 cfg） | test |
| main.rs:1791 | `.map(\ | v\ | v.fallback).unwrap_or(false)` 可一行 `.is_some_and(\ |
| main.rs:1812/1818 | 推荐数 6 写死两次（`suggest_questions(..., 6)` 与 `qs.len() >= 6`） | 提局部 const | safe |
| main.rs:1911/1921 | handler 里 `serde_json::to_value(&r).unwrap()`：序列化失败 = worker panic 断开连接 | `expect` 注明不变量，或映 500 | safe |
| main.rs:1902 | 用 `msg.contains("无权访问数据源")` 字符串匹配做 403/422 分类，上游改一字文案 403 静默变回 422 | 在 dms_agent 定义错误 kind 判断，不靠文案 | test |
| main.rs:1926-1927 | 两处 `let _ = chat::save_msg(...)` 吞错：会话消息丢失无任何痕迹，与 1852 行自己写的「降级可接受，不可见不行」矛盾 | `inspect_err` + `tracing::warn!` | safe |
| main.rs:1977 | 每次问答打一条 info「关联键已生成」纯噪声（trace_id 已进 query_log 三表） | 降为 `debug!` | safe |
| main.rs:2089 | `list_convs(...).unwrap_or_default()`：PG 挂了前端看到「空会话列表」而非错误 | inspect_err + warn（或映 500） | safe |
| main.rs:2122 | `conv_msgs(...).unwrap_or_default()` 同上：他人/失败都呈现为空会话 | 同上 | safe |
| main.rs:2118-2121 | `conv_owner` 的 `Err`（DB 错）被 `_` 并入 403，而 api_ask（1844-1847）把同样的 Err 映 500——同一函数两种口径 | 拆 `Ok(None)`→403 / `Err`→500 | test |
| main.rs:2134-2135 | `let _ = chat::delete_conv(...)` 后恒返回 `{ok:true}`，删除失败也报成功 | 失败映 500 或至少 warn | test |
| main.rs:2151 | branch 的 500 把 `e.to_string()` 原文吐给客户端，其他端点全是通用文案——内部细节外泄且口径不一 | 通用文案 + warn 记原文 | test |
| main.rs:2192 | graph sync 失败 `Err(_)` 丢原因，status 只剩 `graph_sync_failed`，次日排障无线索 | `Err(e)` 并 `warn!("...: {e}")` | safe |
| main.rs:2194 | 用 `msg.starts_with("ok")` 字符串判断自家格式化结果来选日志级别 | 直接在 match 两个臂里打日志 | safe |
| main.rs:717-719 / 982-984 / 1062-1064 / 1170-1172 | `SourceRegistry::new(cfg.dsn_map())` + set_policy 循环复制四遍 | 抽 `fn build_registry(&cfg) -> SourceRegistry` | safe |
| main.rs:1152 | 未知子命令（如 `meta syn` 拼错）不匹配任何分支后**静默落入服务启动**，判官/脚本会把一个服务器挂在那 | 兜底：`args.len() >= 2` 且未命中则打印用法退出 2 | test |
| main.rs:558 | `std::env::args()` 遇非 UTF-8 argv 直接 panic | `args_os` + 显式报错 | test |
| main.rs:1547-1555 | 企微回调无 `ip_rate_allow`（api_sso 1455 / api_login 1623 都有），code 枚举面不设限 | 补同款 per-IP 限流 | test |
| main.rs:786 | 题库 JSON `serde_json::from_str(&txt)?` 解析失败无文件路径上下文 | `.context(format!("解析题库 {p} 失败"))` | safe |

## answer.rs（44 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| answer.rs:89-92 | `answer()` 对 `question` 无长度上限，超大问题直接进 `user_prompt` 拼进 LLM 请求（成本/超时面），本层无防御 | 若上游 server 已限长则在此加 debug_assert 或注释指明；否则加 `q.chars().count() > N → BadInput` | test |
| answer.rs:129 | `respond` 文档注释说 vec_down 来自「`retrieve::search_with_status` 的第二项」，但实际调用链是 `run` → `search_report().vector_degraded`（retrieve.rs:302/529），注释指错了来源函数 | 改为「`SearchReport::vector_degraded`（`search_with_status` 的第二项同源）」 | safe |
| answer.rs:142-144 | 向量降级 warn 只有一句定文案，无 space/hits 数/问题摘要，无法与当次问答关联定位 | 加结构化字段 `space = space.unwrap_or("*"), hits = hits.len()` | safe |
| answer.rs:151 | `Some(0.1)` 温度是裸魔数，全文无注释说明为何 0.1 | 提为 `const ANSWER_TEMPERATURE: f32 = 0.1;` 并注一句「引用式回答压低发散」 | safe |
| answer.rs:173 | 「模型未给出带角标的结论」warn 只有 hits 数，没有 question/trace 线索；`respond` 收不到 trace_id，日志无法拼回当次问答 | 把 `&trace_id` 透传进 `run`/`respond`，warn 加 `trace_id` 字段（纯日志增强，不动行为） | safe |
| answer.rs:188-193 | `user_prompt` 先把 `wrap_untrusted(hits)` 物化成 String，再被外层 `format!` 整体拷贝一次，双倍分配 | `wrap_untrusted` 改为向 `&mut String` 写入（或 `write!` 进同一 buffer），省一次全量拷贝 | safe |
| answer.rs:200-202 | `line.trim()` 在同一行被调 3 次（两次在 200-201，一次在 202），逐行重复 trim | `let t = line.trim();` 一次，后续复用 | safe |
| answer.rs:200-202 | `heading` 按 char 计数后直接用作 `t[heading..]` 的字节下标（200 行 `.chars().take_while` → 202 行字节切片），靠「# 是 ASCII 所以 char 数==字节数」才成立，无注释 | 加一行注释钉住这个不变量（同 refs() 967 行已有的注释风格） | safe |
| answer.rs:201-205 | 标题判定要求 `## ` 后必须是 ASCII 空白；模型偶发输出 `##证据`（无空格）时不被识别为内部章节，该节正文只能靠 211 行 score-line 规则逐行兜底，存在泄漏缝隙 | `is_whitespace` 判定放宽为「非空白也接受」会破坏正常标题，故改为：`##证据` 形态也进 `is_internal_heading` 判定（nth(heading) 取不到空白时仍查标题词表） | test |
| answer.rs:211,216 | `out.join("\n").trim().to_string()`：join 分配一次、trim 后再 to_string 又分配一次（同形态还在 481、510、815） | 先对首尾空行裁剪再 join，或 `let s = out.join("\n"); s.trim().to_string()` 至少省不掉的，可接受——更干净的是 trim 后收集 | safe |
| answer.rs:220 | `is_internal_heading(title)` 里 `title.trim()` 冗余：调用点 202 行已经 `line.trim()[heading..].trim()` | 去掉内层 trim 或注释说明防御意图 | safe |
| answer.rs:241 | `is_internal_score_line` 每行 `to_ascii_lowercase()` 分配一个 String；中文标记（检索分数/向量得分/召回分数）根本不需要 lowercase | 拆成两趟：ASCII 标记在 lowered 上查（只对含 ASCII 字母的行 lower），中文标记直接在原行 `contains` | safe |
| answer.rs:242 | `trim_start_matches` 的剥字符集含 `' ' '-', '*', '+', ' | '` 但不含 `'\t'` | 加 `'\t'`（或 `is_whitespace` + 符号） |
| answer.rs:268-270 | `code.to_ascii_uppercase()` 对每个 `[...]` 片段分配 String 仅为前缀比较 | `code.len() >= 3 && code[..3].eq_ignore_ascii_case(prefix)`，零分配 | safe |
| answer.rs:281-288 | `strip_bare_internal_codes` 先整行 `to_ascii_uppercase()`（分配），循环内又对 `upper[at..]` 做 3 次 `find`（每前缀各扫一遍），含多个 code 的行退化为 O(3·n²) | 单趟扫描：逐字节找 `'K'/'S'/'C'`（含小写）再匹配后缀；或至少 `memchr` 式合并 | safe |
| answer.rs:300-302 | `line.as_bytes()[end].is_ascii() && line.as_bytes()[end].is_ascii_alphanumeric()`——`is_ascii_alphanumeric` 蕴含 `is_ascii`，前一条件冗余 | 删掉 `is_ascii()` 判断（保留 `matches!(b'-' \ | b'_')` 分支） |
| answer.rs:369 | `used.iter().position(\ | x\ | x == k)` 线性查；`used` 已 sort+dedup（360-361） |
| answer.rs:370 | 每个角标一次 `format!("[^{}]", i+1)` 分配 | `out.push_str("[^"); out.push_str(itoa 或手工); out.push(']')`；或预先一次性格式化 used 表 | safe |
| answer.rs:380-404 | `wrap_untrusted` 从 `String::new()` 起步且每 hit 一次 `push_str(&format!(...))` 临时分配；hits 的 text 长度已知可预估容量 | `String::with_capacity(hits.iter().map(\ | h\ |
| answer.rs:440-442 | `esc` 四个 `replace` 串行 = 4 趟扫描 + 最多 4 次分配（每块正文+每个 source 都走） | 单趟 `chars().for_each` match 推送 `&amp;/&lt;/&gt;/&quot;`，一趟一分配（保持 `&` 最先的语义等价） | safe |
| answer.rs:446,454 | `clip` 先 `chars().count()` 全扫一遍再 `take(1200).collect()` 扫前半；未截断时 `text.to_string()` 再全量拷贝一次 | 用 `char_indices().nth(BLOCK_CHARS)` 一次定位截断点，未超长直接 `text.to_string()`，超长时省 count 全扫 | safe |
| answer.rs:464-471 与 492-497 | 空行折叠逻辑（「保留但不许连续两个」）在 `keep_cited_only` 与 `keep_supported_only` 逐字重复 | 抽 `fn push_blank_once(out: &mut Vec<String>)` | safe |
| answer.rs:543-547 | 表头判定向后 `find` 第一个非空行当分隔符——跳过了空行；Markdown 里表头与分隔符之间不允许空行，跳过空行会把远处无关的 `\ | --- \ | ` 认成本表分隔符 |
| answer.rs:556-559 | 分隔符单元格要求 `len >= 3` 个 `-`；GFM 合法写法 `\ | - \ | `、`\ |
| answer.rs:571-593 | `numbers_supported` 对每条引用句都重算 `numbers(&hit.text)`（同一句引多篇、多句引同篇都重扫全文）；hit.text 可上千字 | 在 `keep_supported_only` 入口预计算 `Vec<(claimed源数字表)>` 按 hit 下标缓存，判定时查表 | safe |
| answer.rs:579 | grounding 源用 `hit.text` 全文，而模型实际只看到 `clip()` 后的 1200 字（383）：截断点之后的数字模型不可能读到，却能通过核验——「模型编了个截断区外的数」会被误认为有据 | 源数字表用 `clip(&hit.text).0`（与 `wrap_untrusted` 同一窗口）计算 | test |
| answer.rs:580-589 | 把 `document_revision/effective_from/effective_to` 的数字并入合法源，无任何注释说明为何治理元数据可作证（与 SYSTEM 段「版本生效期不能替代正文支撑数值」表面张力） | 加注释说明意图（允许模型复述版本号/生效期本身）；或若无意，则剔除 | test |
| answer.rs:598 | `numbers()` 每次调用 `chars().collect::<Vec<char>>()` 全量分配；在 grounding 循环里被反复调 | 改为 `peekable` 迭代器单趟，不落 Vec | safe |
| answer.rs:602 | 有符号数字识别支持 `'-' '−' '+'`，漏了全角 `'＋'`，与 618 行已支持全角逗号「，」不一致 | `matches!(chars[i], '+' \ | '＋' \ |
| answer.rs:618-623 | 数字扫描把 `.` 当中缀吞入：`3.5.6`、`v2.0.1` 这类串会产出 `3.5.6` 这种怪 token（split_once 只处理第一个 `.`），归一化结果不可预期 | 吞第二个月 `.` 时停止本轮数字（小数点只允许一个） | test |
| answer.rs:660-662 | `strip_ordered_list_marker` 用 char 数 `digits` 直接切 `&s[digits..]`——同 200 行问题，靠 ASCII 才安全且无注释 | 加注释钉不变量（或改 `char_indices`） | safe |
| answer.rs:696-707 | 内层 `hits.iter().any(...)` 里对每个 other 重算 `textual_version_group(other)`（含 lowercase + 8 次 replace 的字符串分配），O(n²) 次分配；712-744 第二趟又把 group/class 全部重算一遍 | 进函数先一次性预计算 `Vec<(group, class)>`，两趟循环都查表 | safe |
| answer.rs:757 | 单行 110+ 列的长链式调用，与全文件风格（80-100 列）不一致，diff/评审噪音 | 折行（纯格式） | safe |
| answer.rs:818-826 | `textual_version_class` 对 `hit.text` 全文跑 8 次 `contains`（4 旧 + 4 新），每个 hit 每次调用都重扫；配合 697 行的内层重复调用放大成 O(n²·文本长） | 与 696 行合并：预计算阶段各扫一次；或单趟多模式扫描 | safe |
| answer.rs:818-839 | 正文层标记判定过于宽松：现行制度正文常写「原《XX办法》同时废止」——单含「废止」即把该 hit 判为「旧版」，可能误触发版本冲突兜底 | 正文命中「废止」时要求与「新版/现行」共现才计旧版，或正文只认「旧版/历史版/历史口径」、「废止」只认文件名层 | test |
| answer.rs:848-850 | 连续 8 次 `normalized.replace(marker, "")`，每次一趟扫描一次分配 | 单趟手工过滤或折叠为一轮 scan | safe |
| answer.rs:851-856 | `filter(is_alphabetic)` 把数字全剥掉：「制度A1」与「制度A2」归一成同名组，可能误配版本对（注释只说防全局误配，没提数字剥离的反向风险） | 保留数字参与分组（只剥空白与标记词），或注释说明为何剥数字利大于弊 | test |
| answer.rs:852 | `normalized.chars().count()` 对刚生成的 String 又全扫一遍；852 行同句里 `contains(&normalized.as_str())` 精确匹配 5 个泛名 | 在 filter 时顺手计数；泛名判定保持不变 | safe |
| answer.rs:859-871 | `governed_version_signature` 每字段一次 `format!`，再 collect+join，每个 hit 重复调（696/718 两处循环内） | 与 696 行同一轮预计算缓存；或直接 `write!` 进复用 buffer | safe |
| answer.rs:886-889 | `table_cell` 三次 `replace` 三趟扫描（每个冲突表格单元格都走） | 单趟 chars 循环推送替换 | safe |
| answer.rs:907-921 | `strip_refs` 末尾 `out.chars().filter(...).collect()` 再走一趟全量分配；且字符白名单 `"-*•.。：:、"` 是裸字面量，语义（「列表符号与句读」）只在 906 行注释里 | 过滤并入主循环；字面量提为 `const SHELL_CHARS: &str` 并注释 | safe |
| answer.rs:940 | 角标跟随判定只跳过 ASCII 空格（`trim_start_matches(' ')`）；模型写 `。\t[^1]` 或全角空格 `。　[^1]` 时角标被切进下一句 → 正句被剔 | 改 `trim_start_matches(char::is_whitespace)`（全角空格如有需要再列） | test |
| answer.rs:970 | `[^01]` 这种带前导零角标 parse 成 1 被当合法引用，与模型输出习惯的「`[^1]`」契约无注释说明；溢出（`[^999…9]`）时 parse 失败静默丢弃 | 加注释说明两种边缘形态的处理取向（接受前导零、丢弃溢出），或溢出时 warn 一次 | safe |
| answer.rs:1054,1081 等 | 测试里 `respond(&f, &[], ..., Instant::now(), ...)` 逐次手写 7 参调用，新增参数时全测试面都要改（已出现 8+ 处重复形态） | 抽测试辅助 `fn call(f:&Fake, hits:&[Hit]) -> (Result<Answer,KbError>, Obs)` | safe |

## kb_api.rs（43 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| kb_api.rs:23,308,575 | 许可数 `4` 在 `const_new(4)` 与两条 429 文案「同时最多 4 个」三处硬编码，改许可数文案必漂 | 抽 `const UPLOAD_PERMITS: usize = 4`，信号量与文案（`format!`）共用 | safe |
| kb_api.rs:308,574-576 | 429 文案整段重复两处 | 抽 `fn upload_gate_full() -> ApiErr` 或常量文案，两处调用 | safe |
| kb_api.rs:2033-2043 vs 322 | **潜在 bug**：`read_form` 只解析 `file/space_id/folder_id`，multipart 里的 `preset` 字段从未入 `f.q`，导致 `upload` 的 `UploadReq.preset`（322 行）恒为 `None`——行 88 注释宣称可选的分块策略在上传入口是死参数（`ingest_url` 的 JSON 路径 585 行却有效） | `read_form` 加 `"preset" => f.q.preset = text(field).await`，补一条 multipart 解析钉测 | test |
| kb_api.rs:2049-2051 | `text()` 把字段读取错误 `.ok()` 吞成 `None`，读失败与未提交不可区分 | 失败时 `tracing::warn!` 一记再按缺省处理 | safe |
| kb_api.rs:2050 | 表单文本值不 trim：`space_id="  "` 会原样当空间 ID 用，走到 403 才暴露 | `filter` 前加 `trim()`，空白串按缺省（与 2048 行注释口径一致） | test |
| kb_api.rs:444-446 | `reprocess` 整文件 `fs::read` 入内存，既不占 `UPLOAD_GATE` 许可也无读取超时——N 个并发重处理绕过了 upload 精心设的内存闸 | 复用 `UPLOAD_GATE.try_acquire()` + 读超时（与 upload 同序），429 语义不变 | test |
| kb_api.rs:453 | **潜在 bug**：`reprocess` 固定 `preset: None`，原文档若按 `qa`/`laws` 等非 general 策略上传，重处理会静默改用 general 重新分块 | preset 持久化到 doc 行或 reprocess 时回读原 preset；至少注释说明「重处理不保留原策略」是有意为之 | test |
| kb_api.rs:468 | `register_source(...)` 返回值被静默丢弃（upload 侧 345 行会据此清 `out.source`），无 `let _ =` 也无注释说明此处为何可忽略 | 改 `let _ = register_source(...)` 并加一行注释（响应由 `doc_json(&row)` 重建、不带 datasource，故返回值无消费者） | safe |
| kb_api.rs:1729 | `download_doc` 整文件读入内存，无并发闸、无大小帽、无流式——20MB 文件 × N 并发下载与 upload 闸防的是同一个问题 | 至少加并发闸/尺寸上限；理想改 `tokio::fs::File` + 流式 Body | test |
| kb_api.rs:494-496 | `set_enabled` 已提交后 `sync_source_state` 失败返回 500：调用方看到「失败」但文档状态其实已翻转，重试又返回 ok，状态面自相矛盾 | 同步失败降级为 `tracing::warn!` + 响应带 `source_synced: false`；或至少在 500 文案里说明「文档状态已变更，数据源同步失败」 | test |
| kb_api.rs:530 | `update_doc_metadata` 无条件 `sync_source_state`：非表格文档（无上传数据源）每次改元数据白打一条 UPDATE；且与上一条同款的「已提交又报 500」部分失败 | 仅当文档存在上传源时再同步（可先查 `get_doc`/ds 存在性），或同步失败只 warn | test |
| kb_api.rs:369 | `spaces`（GET）每次调用都 `ensure_space` 写一条幂等 INSERT——读端点上每次必写 | 先 `list_spaces`，缺个人空间再 `ensure_space` 后重列（常见路径省一次写） | test |
| kb_api.rs:1564-1565, 914-916, 1701-1702 | `docs`/`export_space`/`doc` 里 2~3 条相互独立的 DB 查询串行 await，白白累加 RTT | `tokio::join!` 并发（同一池、无依赖、错误各自 map） | safe |
| kb_api.rs:1564 + store.rs:1686-1694 | `docs` 的 `list_docs` 无 LIMIT 无分页，大空间一次吐出全量 DocRow JSON；而 Y7 导出（914-915）已证明需要分页口径 | 给 `docs` 加与 export 相同的 `limit/offset`（缺省保持现状只封顶），或注释说明空间文档数有别的上界保证 | test |
| kb_api.rs:916-917 + store.rs:440-463 | `export_space` 把 `list_folders` 全量拉回来再 `truncate(2000)`——SQL 无 LIMIT，且每行带两个相关子查询（child_count/doc_count），超帽部分纯浪费 | 在 export 专用查询里把 `LIMIT 2000` 下推（共用 `list_folders` 的 `docs`/`folders` 端点不受影响） | test |
| kb_api.rs:536-544 | **注释与代码不符**：「本轮不注册 main.rs，集成时各加一行」「接线前整块属未达代码：allow 挂子模块」均已过时——`main.rs:1327-1329` 已注册三条路由，且全文无任何 `#[allow]`；1175-1191 行的测试还把过时注释钉成了契约 | 更新块注释为「已接线」口径，删除「allow 挂子模块」句；测试改为断言 main.rs 存在注册行（或保留字面钉测但同步改注释文本） | safe |
| kb_api.rs:573 vs 306 | 两条入库链认证口径不一：`upload` 用 `session_viewer`（只认 header 会话，刻意拒绝 body 身份回退），`ingest_url` 用 `manager_viewer`（接受 JSON body 的 login_name 回退）；注释只说「同序」不提这一差异 | 二者对齐（ingest_url 是 JSON body 可预读，完全可以走 `session_viewer`），或注释明说差异是有意的 | test |
| kb_api.rs:1230,1249,1288 | `grant_space`/`grant_roles` 返回 `(StatusCode, ApiOk)` 元组但状态码恒为 `OK`，类型系统白绕一圈 | 返回类型简化为 `Result<ApiOk, ApiErr>`（wire 形状不变：200+body） | safe |
| kb_api.rs:1250 vs 1289 | 单授权响应 `"succeeded": 1`（数字）与批量 `"succeeded": [...]`（对象数组）同名字段两种形状，前端要双写解析（wire 冻结，仅记录在案） | 不改 wire；在 API 文档/前端注释中写明两种形状，或下次协议版本统一为数组 | test |
| kb_api.rs:1374 + 1388 | `validate_grantee` 角色分支为校验一个 code 拉全表（LIMIT 500）角色目录；且 DMS 角色超 500 时目录被截断，合法角色会被误判「不存在」 | 改点查 `SELECT 1 FROM t_role WHERE TRIM(role_code)=? LIMIT 1`（顺带解决截断误判） | test |
| kb_api.rs:1398-1403 | `dms_role_options` 去重时 `seen.insert(role_code.clone())` 对每行都克隆一次 code | `HashSet<&str>` 借 `rows` 判重，最后构造时只克隆一次 | safe |
| kb_api.rs:1310-1311 | `validated_role_codes` 每个唯一 code 分配两次 String（seen 一次、out 一次） | `seen: HashSet<&str>` 借用判重，`out` 里仅一次 `to_string()` | safe |
| kb_api.rs:1421-1424,1462-1465 | `register_source` 里「get_doc + space_writable.unwrap_or(false)」撤权复核块逐字重复两遍 | 抽 `async fn still_writable(st, v, doc_id) -> bool`，两处调用 | safe |
| kb_api.rs:432-434 | `is_tabular` 每次调用对文件名做全量小写化 + 4 次 `format!(".{ext}")` 分配；且这份扩展名清单是 knowledge 表格类型集的第二份真相源（违背本文件头「零业务判定」原则） | 本地改写：`let n = row.name.to_ascii_lowercase(); [".csv",".xls",".xlsx",".xlsm"].iter().any(\ | e\ |
| kb_api.rs:296-297 | `is_safe_source_uri` 为判前缀把最长 500 字符的 URI 整体小写化分配一次 | `uri.get(..8).is_some_and(\ | p\ |
| kb_api.rs:837 | `url_file_name` 循环内 `slug.chars().count() >= 60` 每字符重扫全串（O(n²)，n≤60 但模式难看） | 循环外维护 `len: usize` 计数器 | safe |
| kb_api.rs:1036 | `sanitize_description` 同款：`out.chars().count() >= DESC_MAX_CHARS` 每字符重扫 | 维护字符计数器 | safe |
| kb_api.rs:1775 | `percent_encode_filename` 每字节一次 `format!("%{b:02X}")` 堆分配（CJK 文件名每字符 3 次） | `use std::fmt::Write; write!(out, "%{b:02X}")` 直接写入 | safe |
| kb_api.rs:2115 | `doc_json` 内 `chrono::Local::now()` 逐文档取本地日期——`docs`/`export` 列表 N 个文档 N 次时区系统调用 | 在处理函数入口取一次 `today`，经参数传入 `doc_json`/`doc_quality` | safe |
| kb_api.rs:2238-2246 | `remove_files` 把 `read_dir` 失败静默 return、`remove_file` 失败 `let _` 吞掉——孤儿文件无声累积（delete 路径 1804 行唯一的观测点被绕开） | 两处失败各加 `tracing::warn!(doc_id, err)`，语义不变 | safe |
| kb_api.rs:2249-2257 | `cleanup_source` 首个失败即 early-return：物理表清理失败时，数据源注册行与 schema 文档两行孤儿被跳过（与 delete 注释「幂等回收」的承诺不齐） | 三步各自 best-effort 收集错误，最后汇总返回/告警 | test |
| kb_api.rs:2015-2016 | `chunk` 响应同时带整段 `text`（join）与逐块 `chunks`（含全文），正文载荷约 2×（wire 冻结，记录在案） | 不改 wire；确认前端只用其一后下版本下线另一份 | test |
| kb_api.rs:1969 | `span = Some(1)` 落入 `_ => window` 分支——客户端按 `Citation.span=1` 回查单块时拿到的是 window=1 的三块上下文，语义偷偷换了 | 改 `Some(n) if n >= 1 => span(...)`（`retrieve::span` 内部已 clamp(1,16)） | test |
| kb_api.rs:1927 vs 1843 | `ask` 不 trim/不校验空问题（靠 knowledge 层 BadInput「问题为空」兜底 400），`search` 在边缘用 `nonempty_question`（400「问题不能为空」）——同族端点两种校验点两种文案 | `ask` 复用 `nonempty_question`，文案统一 | test |
| kb_api.rs:1843-1844 | `search` 先校验问题再认证：未认证空问题拿到 400 而非 401，与文件内其他端点「认证优先」的次序不一致 | 两行对调（先 `viewer` 后 `nonempty_question`） | test |
| kb_api.rs:94,395-397 | `CreateSpaceReq.name` 无 `#[serde(default)]`：缺 `name` 字段走 axum 反序列化 422 通用文案，到不了 395 行精心写的 400「名称不能为空」 | 给 `name` 加 `#[serde(default)]`，让空名落到友好 400 | test |
| kb_api.rs:115 vs 122-123 | `DocMetadataReq.tags` 无 `#[serde(default)]` 而 `related_doc_ids` 有：缺 `tags` 字段 422、缺 `related_doc_ids` 正常——同结构体内两种缺省口径 | `tags` 补 `#[serde(default)]` | test |
| kb_api.rs:402-409 | 碰撞检查用 `SELECT COUNT(*)` + `fetch_optional`——COUNT 恒返一行，`fetch_optional` 语义错位；存在性判定也用不着全表计数 | 改 `SELECT 1 ... LIMIT 1`（`fetch_optional` 语义恰好）；或保留 COUNT 改 `fetch_one` | test |
| kb_api.rs:446,1731 / 76,994 / 492,980,1685 / 911,1562,1598 | 「文档文件暂时不可读取」「知识库服务暂时不可用，请稍后重试」「无权修改空间 {} 的文档」「无权访问知识空间」等文案多处逐字重复 | 抽常量/小函数（如 `fn kb_unavailable() -> ApiErr`），一处改文案全族生效 | safe |
| kb_api.rs:168,218 / 185-187,219-221 | 「未认证：缺会话 token 或 login_name」与 `load_principal` 的 403 映射在 `viewer`/`manager_principal` 各写一份 | 抽 `fn unauthenticated() -> ApiErr` 与 `async fn principal_or_403(...)`，两处复用 | safe |
| kb_api.rs:505-510 | `update_doc_metadata` 为构造一次性 `KbQuery` 克隆 `login_name`/`role_code` 两个 String（其余 handler 都是 move） | 改用 `manager_principal(&st,&headers,&req.login_name,&req.role_code)` + `Viewer::new(...)`（即 `manager_viewer` 的展开），零克隆 | safe |
| kb_api.rs:720-726 | `fetch_url_guarded` 每跳重建 reqwest Client（TLS 配置/连接池初始化），重定向链成本翻倍；单跳场景也付一次构建费 | 注释已说明 resolve 钉定必须每跳专用 client，可接受；若要优化可按 `(host, addrs)` 做小型缓存，先加注释说明现状是刻意权衡 | safe |
| kb_api.rs:809-810 | `looks_like_html` 先 `collect::<Vec<u8>>` 再 `from_utf8_lossy` 两次分配，仅判 512 字节前缀 | `String::from_utf8_lossy(&bytes[..bytes.len().min(512)])` 一次拷贝后小写化比较 | safe |

## KbGraph.vue（42 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| KbGraph.vue:658-677 | `resetGraph` 成功路径：`await reload()` 内 `++graphEpoch`(758)，finally 里 `epoch === graphEpoch` 永假 → `resetting` 永远 true，按钮永久「清空中…」禁用 | 在 `await reload()` 前先 `resetting.value = false`，或 finally 无条件复位（reload 已保证后续状态正确） | test |
| KbGraph.vue:683-718 | `reconcileGraph` 同款 bug：713 行 `await reload()` 后 epoch 失配，`reconciling` 永久 stuck，「修复图谱」按钮变砖 | 同上，reload 前复位 `reconciling` | test |
| KbGraph.vue:671-672,711-713 | 「图谱已清空。」「修复完成：…」设置后紧跟 `await reload()`，而 reload 在 762 行 `note.value = ''` → 成功文案被立即抹掉，用户看不到结果 | 把成功文案移到 reload 之后设置，或给 reload 加 `keepNote` 参数 | test |
| KbGraph.vue:596-616 | `build()` 竞态：`building.value = true` 在 await 之后（609 行）才置位，快速双击会发出两个 POST；对比 mindmap regenerate(305) 是 await 前置位，写法不一 | 在 fetch 前同步置 `building`（或加 pending 闸） | test |
| KbGraph.vue:568-574 | 轮询一次瞬断（网络抖动）就走 catch：`building=false`+「状态查询暂不可用」，但服务端构建仍在跑，进度 UI 永久丢失（resumeBuilding 只在 reload 时执行） | catch 里做有限次重试（如连续 3 次失败再放弃），或保留 building 并降频轮询 | test |
| KbGraph.vue:723-747 + 757-778 | 换空间发生在 expandNeighbors 途中：finally 的 epoch 检查使 `expanding` 不复位，而 reload() 不清 `expanding/reconciling/resetting/failedLoading` → 「展开邻居」永久禁用 | reload() 里统一复位所有在途标志位 | test |
| KbGraph.vue:62-63,509-515,568-574 | token 缺失时 headers() 抛出「登录会话已失效」，但 loadSubgraph/pollStatus 的 catch 把它显示成「图谱暂不可用/状态查询暂不可用」——文案与真实原因（需重新登录）不符 | catch 里识别 headers 抛出的特定错误，note 直接透出其 message | test |
| KbGraph.vue:26-27,194-243 | nodes/edges 是深响应式，tick 里每帧对 ~800 节点 × x/y/vx/vy 走 proxy setter 触发依赖通知，纯浪费 | `toGNode` 返回 `markRaw(...)`（模板只读 label/type/weight，无需节点级响应） | test |
| KbGraph.vue:304-313,358-359 | 有 focus 时 `isDimmed` 对每个节点 `edges.some(...)`，render 每帧 O(N·E)（800 节点时数十万级/帧） | render 开头按 focus 算一次邻接 Set，isDimmed 改 O(1) 查表 | safe |
| KbGraph.vue:89 | `Number(...) \ | \ | 1`：服务端显式给 weight=0 会被改成 1，与「Math.max(0, …) 允许 0」的写法自相矛盾 |
| KbGraph.vue:268,270 vs 522 | 搜索用 `toLocaleLowerCase()`（土耳其语 locale 下 i/I 匹配出错），同文件 522 行却用 `toLowerCase()`，两处口径不一 | 统一为 `toLowerCase()` | safe |
| KbGraph.vue:281 | 图例 `.slice(0, 12)` 截断后无任何「等 N 类」提示，用户无从得知被截 | 超出时末尾加一行 `+N 类` | safe |
| KbGraph.vue:311-312 vs 318-319,464-465 | `isDimmed`/`selectedNeighbors` 里 `nodes.value[e.source].id` 不用可选链，`edgeDimmed` 却用 `?.`——同款访问两种写法，索引失配时前者直接抛 | 统一加 `?.` 并判空 | safe |
| KbGraph.vue:336,782-795 | 主题只在 render 时读 `dataset.theme`；力导冷却（alpha 低于阈值）后切主题，画布颜色一直停留旧主题直到下次交互 | onMounted 里用 MutationObserver 监听 documentElement 的 data-theme 变化触发 render() | safe |
| KbGraph.vue:355,382 | 边标签 `slice(0,12)`、节点标签 `slice(0,10)` 截断后不加省略号，用户误以为是全名 | 截断时拼 `…` | safe |
| KbGraph.vue:401,441 + 434-442 | 只绑了 pointerdown/move/up：`pointercancel`（触摸被打断）后 drag.mode 滞留 'node'/'pan'；且 `releasePointerCapture` 在未持捕获时会抛 DOMException | 加 `@pointercancel` 复位 drag；release 前 `hasPointerCapture` 判断或 try/catch | safe |
| KbGraph.vue:434-438 | 只有点中节点才切换选中；点画布空白（pan 未移动）不清除选中，与常规图交互习惯不符，详情卡只能点×或再点节点 | pointerup 时 `drag.mode==='pan' && !drag.moved` → `selectedId=''` | test |
| KbGraph.vue:404-413,435 | `drag.moved` 在任何 pointermove 即置 true：点击时手滑 1px 就丢失「点选」语义 | 以位移阈值（如 3px）判定 moved | test |
| KbGraph.vue:396-399 | 抓取节点瞬间 `node.x=point.x` 直接把节点中心吸到指针，节点「跳」一下 | mousedown 时记录指针与节点中心偏移，move 时减去 | test |
| KbGraph.vue:464-465,896 | 邻居 key 为 `relation-label`：服务端若返回双向同关系边（A→B、B→A 同 label），同实体详情里出现重复 key，Vue 报警且渲染两行重复 | selectedNeighbors 按 `relation+label` 去重，或 key 加序号 | safe |
| KbGraph.vue:508 | `alpha = 1; if (!raf) raf = ...` 与 `wake()`(189-192) 逻辑重复但写法不一 | 改 `wake(1)` | safe |
| KbGraph.vue:641 | `failedOffset` 信任服务端回显（缺省 0）：服务端不回 offset 字段时，第 2 页数据配第 1 页的页码显示 | 缺省回退为本次请求的 `offset` 参数 | safe |
| KbGraph.vue:697-700 | `Number(plan.orphan_chunks ?? 0)`：服务端返回非数字字符串得 NaN，`!NaN` 为 true → 误判「无需修复」 | `Number(...) |  |
| KbGraph.vue:740 | `added===0` 时报「没有更多邻居」，但本次可能合并了新边（节点都在、边新增）——文案不精确 | 让 mergeSubgraph 同时返回新增边数，文案区分 | safe |
| KbGraph.vue:775-777 | loadSubgraph 失败（unavailable=true）后仍 loadStats/resumeBuilding：画布显示「暂不可用」而工具栏出现统计数/进度条，状态自相矛盾 | unavailable 时跳过 stats 与 resume | safe |
| KbGraph.vue:810 | `statRelations ?? 0`：关系数缺失时显示「关系 0」，把「未知」伪装成「0」 | 两者任一缺失就整段不显示，或显示 `—` | safe |
| KbGraph.vue:810 vs 855 | 工具栏「实体 X·关系 Y」（全空间口径）与左下角「实体 N·关系 M」（画布口径）并排出现、数字不同，无标注区分 | 文案区分：「全量实体 X·关系 Y」/「画布实体 N·关系 M」 | safe |
| KbGraph.vue:816 vs 826,831 | 「构建中」无省略号，「清空中…」「修复中…」有；mindmap 441/452「导出中」「生成中」也无——同状态文案两种风格 | 统一（建议都带 …） | safe |
| KbGraph.vue:834-837 | 进度条容器只有 role="status"，条本身无 role="progressbar"/aria-valuenow；percent 未知时假显示 12%(835) 对读屏无说明 | bar 加 role="progressbar" + :aria-valuenow；未知时 aria-busy + 不定态动画 | safe |
| KbGraph.vue:841-846 | canvas 只有 aria-label 无 role（读屏不一定播报），且拖拽/缩放/点选全无键盘替代 | 加 `role="img"` 与兜底文本；并补 +/- 缩放、重置视角按钮（对触屏也有用） | test |
| KbGraph.vue:852 | 「接口上线后会自动展示」——实际无任何自动重试，必须刷新或切空间，文案过度承诺 | 改为「接口上线后刷新页面即可展示」或加定时重试 | safe |
| KbGraph.vue:771-774 + 850-853 | 未选空间（spaceId 空）走 unavailable 分支，文案却是「服务端图谱接口尚未就绪」——原因不符 | unavailable 文案按 spaceId 是否存在区分（「请先选择知识空间」） | safe |
| KbGraph.vue:859,885 vs KbMindmap.vue:507 | 失败块/详情两个 aside 无 role；mindmap 卡片有 role="dialog"——同款浮层三种写法 | 统一补 role（dialog/group） | safe |
| KbGraph.vue:867 | `:key="item.chunk_id"`：若 chunk_id 仅文档内唯一，跨文档撞 key | key 改 `${item.doc_id}-${item.chunk_id}` | safe |
| KbGraph.vue:868,983 | kind 只特判 'failed'，其余一律显示「待建」但 data-kind 用原值——出现第三种 kind 时文案与样式（warning 底色）错位 | kind 白名单映射，未知 kind 用中性样式 | safe |
| KbGraph.vue:466,895-899 | 邻居只取前 8 条（466 行硬编码）且无任何截断提示，关联数 30 也只看到 8 个 chip | 末尾加 `等 N 个` 或 title 提示 | safe |
| KbGraph.vue:914 | `.graph-head span` 选择器过宽：命中工具区里的 `.graph-stats`/`.graph-hits`（在 .graph-head 内），其 font-size 被 11.5px 覆盖、外加 margin-top:3px，统计文字轻微错位 | 改为 `.graph-head > div > span` 或给描述 span 加专用 class | safe |
| KbGraph.vue:936 + 671,701,711 | `.graph-note` 固定 warning（黄底）样式，但「图谱已清空」「修复完成」「无需修复」是成功/中性信息，成功也用警示色 | note 加 type（success/info/warning）状态色 | safe |
| KbGraph.vue:76-77 vs 93 | 注释称「label 在契约里是实体类型，不是显示名」，但 93 行在 name 缺失时恰恰拿 label 当显示名——注释与代码不符 | 注释补一句「name 缺失时回退用 label 作显示名」 | safe |
| KbGraph.vue:199-243,339,351,494,555-556,592,733 | 魔法数成堆：160000 截断、0.86 阻尼、260/40 标签阈值、limit 200/120、10min、2000/1200ms——mindmap 有 ROW_H/COL_GAP 常量风格可对照 | 提为具名常量（比照 FAILED_PAGE/MAX_CANVAS_NODES） | safe |
| KbGraph.vue:628-647,649-652 | loadFailed/toggleFailed 前不清 note：打开抽屉时残留上一条操作提示（如「图谱已清空」），语境错乱 | toggleFailed 打开时 `note.value = ''` | safe |
| KbGraph.vue:317 | 搜索时 `edgeDimmed` 对所有边一律 dim——包括两端都命中的边，丢失「命中路径」信息 | 两端都在 matchSet 的边不 dim | safe |

## ask.rs（40 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| ask.rs:237-244 | DESTRUCTIVE 中 `"drop"`/`"truncate"` 无词边界，英文混入问句（如 "dropdown"、"waterdrop"）会被误判红线反问 | 词表命中后加 ASCII 字母边界检查（前后邻字符非 ascii_alphabetic），行为变化需配正反例测试 | test |
| ask.rs:261 | fast 判 answer 后每问必走 `registry_hit`（最多 3 条召回 SQL），结果在同一问句+同一 ds 内可缓存却无缓存 | 与 `compute_scope_cached` 同思路加短时缓存；改动命中时机需测试 | test |
| ask.rs:339-341 | ASKING 含单字 `"几"`/`"前"`，「几乎」「目前」「之前」等常用词全会被当成「疑问词」放行，覆盖兜底（②b）对这族问句失效 | 单字词改双字以上或加邻字约束；判据变化需配测试（注意 test at 1547 同步） | test |
| ask.rs:348-350 vs 406-411 | 注释自称「两处必须同一份」，但 `TOPIC_SYSTEM` 硬编码 9 个主题，`KNOWN_TOPICS` 已 13 个（缺 开票/对账/业务员/仓库）——漂移已经发生 | `TOPIC_SYSTEM` 改 `format!` 注入 `KNOWN_TOPICS.join("、")`（与 729-737 同法）；prompt 文案变化需测试 | test |
| ask.rs:419 vs 728 | 两个函数各自定义 `const TIMEOUT: Duration::from_secs(4)`，同值两处 | 提为模块级 `const FAST_CALL_TIMEOUT` | safe |
| ask.rs:443-475 | `parse_clarify_options` 魔法数 12/4/60/4/2 散落（457/460/467/471），与本文件 `REFS_FRAG_MAX_CHARS` 等具名常量风格不一致 | 提具名常量（`LABEL_MAX_CHARS` 等），纯改名 | safe |
| ask.rs:455 | `q.trim().trim_matches('"').trim_matches('"')` —— 两个 `trim_matches` 字符完全相同（0x22），第二个是死调用；若本意是弯引号 `“”` 则根本没剥到 | 改为闭包 `matches!(c, '"' | '“' |
| ask.rs:479-516 / 521-558 / 900-931 | 三处近乎逐字段相同的空 `AskResult` 脚手架（15+ 字段重复三遍），加字段时三处易漏 | 抽 `fn empty_reply(route, elapsed_ms, note) -> AskResult`，三处各自覆写差异字段；字段值逐一保持不变 | safe |
| ask.rs:503-505 / 532 | 用户问句原文无限长插进用户可见 `caliber_note`；本文件 refs 有 500 字纪律，文案出口没有 | 插入前 `chars().take(N)` 截断；文案变化需测试 | test |
| ask.rs:547-548 / 737 | `KNOWN_TOPICS.join("、")` 每次调用重新分配同一字符串（no_topic_reply 与 fast prompt 两处） | `OnceLock<String>` 或 `concat!` 字面量缓存一次 | safe |
| ask.rs:594-631 vs 1017-1040 | `out_of_scope_topic` 与 `value_word_residue` 的「剥 consumed→剥 STRIP_WORDS→滤标点→数汉字≥2」整段流水线逐字重复 | 抽共享纯函数（如 `residue_after_strip(s, consumed)`），两函数各有单测可锁行为 | safe |
| ask.rs:599-609 | `consumed: Vec<String>` 对全是 `'static` 的词做 `to_string()`（约 20+ 次堆分配）；`value_word_residue:1019` 同类场景用的是 `Vec<&'static str>`——同文件两种写法 | 改 `Vec<&'static str>`，`replace(w.as_str())` → `replace(w)` | safe |
| ask.rs:611 / 1025 | `sort_by_key(Reverse(w.chars().count()))` 在比较器里反复重算字符数（O(n log n) 次） | 先 `map` 出 `(len, w)` 再排序，或排一次缓存长度 | safe |
| ask.rs:620 vs 1034 | 标点过滤字面量 `"，。？?、,.~～!！:：;；「」『』()（）"` 逐字出现两次 | 提模块级 `const PUNCT_CHARS: &str` | safe |
| ask.rs:624 vs 1036 | `('\u{4e00}'..='\u{9fff}').contains(c)` 汉字计数逻辑重复两处 | 抽 `fn hanzi_count(s: &str) -> usize` | safe |
| ask.rs:669-673 / 994 | `[Dimension::WarZone, Dimension::Region]` 成员维度清单两处各写一份（topic_covered 与 dimension_value_hit），加维度时易漏一处 | 提 `const PROBE_DIMS: [Dimension; 2]` | safe |
| ask.rs:765 | 763 已整串 trim，765 取首行后又 `.trim()`——`str::lines` 已处理 `\r\n`，此处 trim 冗余 | 删 `.trim()`（行为等价） | safe |
| ask.rs:775 | 同 455：`topic.trim_matches('"').trim_matches('"')` 同字符两遍，第二遍死调用 | 同 455 修法 | test |
| ask.rs:799 | `Vec::new()` 后按 router 七位逐个 push，容量已知 | `Vec::with_capacity(crate::ROUTER_ORDER.len())` | safe |
| ask.rs:800 / 1152-1174 | `router(...)` 在 `ask_single` 内构造，复合拆解的每个子问都重建 7 个 Box；成员只持依赖引用不持子问状态 | 在 `ask()` 构造一次，`ask_single` 改收 `&[Box<dyn Answerer>]`；需确认各 Answerer 无 per-call 内部状态（从签名看成立） | test |
| ask.rs:813 vs 836 | direct-doc 命中路径上 `resolve_document` 算两遍（`needs_production_detail_fallback` 一次、`attach_document_identity` 一次） | 解析一次把 `&Document` 传下去 | safe |
| ask.rs:824-827 vs 835-837 | 明细回填早退分支（enriched）跳过了 `attach_document_identity`，而普通 direct-doc 命中会挂单据身份块——两个 direct-doc 出口的 view 元数据不一致 | 确认是否有意；若无，在早退前补 `attach_document_identity` | test |
| ask.rs:849-860 | 布尔形参 `warehouse` 在 856 被同名的 details 绑定遮蔽；853 用字面量 `true` 靠 850 的早退保证等价，读的人要推一遍 | 内层改名 `wh`，853 直接传形参 `warehouse`（行为逐字等价） | safe |
| ask.rs:988 vs 1020 | `dimension_value_hit` 先算 `sales_contract_metrics`，随后 `value_word_residue` 内部又对同一问句重算一遍 | 把已算的 `&hits` 传给 `value_word_residue`（私有函数改签名） | safe |
| ask.rs:1056-1061 | 1052-1054 在 `stem != word` 时已早退，故 1061 的 `!out.contains(&suffixed)` 恒真（死条件） | 删条件直接 push | safe |
| ask.rs:1056-1059 | `_ => String::new()` 通配臂：调用点只传 WarZone/Region，将来给 Dimension 加变体时这里静默产空串 | 加 `debug_assert!(matches!(dim, WarZone | Region))` 或改穷尽匹配 |
| ask.rs:1091-1092 | 探针的 `gate_on(...).ok()?` / `fetch(...).await.ok()?` 把权限注入失败与取数失败都静默吞成 None——与本文件 808 行「权限注入失败是 fail-closed 信号」的纪律形成对照，且零日志 | 失败时补 `tracing::debug!`（保持 None 语义不变） | safe |
| ask.rs:1123 / 1129-1137 | `scalar.then(\ | \ | ...).flatten()` 绕一层 Option<Option<_>> |
| ask.rs:1138 | 明细 SQL 的 `100` 魔法数（detail limit）无说明 | 提 `const DETAIL_ROWS: usize = 100` 或注明与 direct.rs 的同值来源 | safe |
| ask.rs:1187 | 非主源问答每问一次 `get_datasource` PG 查询（`registry.get` 池有缓存、登记行没有） | 加短时缓存或随 registry 缓存；失效语义变化需测试 | test |
| ask.rs:1210-1213 | `is_followup` 的 MARK 含单字 前/后/该/此/它/换：「目前」「之后」「应该」「因此」「其它」「兑换」全部误判追问，14 字内首问会被白送一次改写 LLM | 单字词加邻字约束或换双字形态；判据变化需配测试 | test |
| ask.rs:1258 | 改写 LLM 调用失败 `.ok()` 静默吞错零日志——同文件 426/746/305 同类失败都有 warn | 补 `tracing::warn!(err = %e, "追问改写失败 → 原样放行")` | safe |
| ask.rs:1258 | 改写的 `reply.usage` 被丢弃：全文件其他 LLM 调用都报 `on_usage`（434/754/run.rs:1201），独缺这次——查询日志 token 列少算改写用量，正是文件头 K6-B 那族问题 | `rewrite_followup` 加 `on_usage` 形参（ask() 传 `d.on_usage`）；签名变化需测试 | test |
| ask.rs:1258 | 改写无 `tokio::time::timeout`：triage.rs:83-84 自己写明「fast 自带 90s 超时，等 90s 整条问答都废」，同一论点适用于改写 | 包 4~8s 超时（与 FAST_CALL_TIMEOUT 统一）；超时语义变化需测试 | test |
| ask.rs:1260 | 改写结果只剥直引号 `"` 与 `。`，模型常回的弯引号 `“”`、`「」` 不剥——与 775 行 `parse_gate_verdict` 的剥词集不一致 | 对齐 775 的剥法；输出变化需测试 | test |
| ask.rs:1307 | `section.push_str(&format!("\n{}. {frag}", i + 1))` 每段一次临时 String | `use std::fmt::Write; write!(section, ...)` | safe |
| ask.rs:1320-1321 | `starts_with("select")`/`starts_with("with")` 无词边界：英文词 "selection"/"withdraw" 开头的改写结果会被当 SQL 丢弃 | 判据改 `select ` / `with ` 或词边界检查；判据变化需测试（文档已记漏判方向，这是误判方向） | test |
| ask.rs:1484 | 测试 panic 文案里有一串无意义空格 `"——              那样已经付了..."` | 删多余空白 | safe |
| ask.rs:1547-1552 | 测试 `asking` 闭包把 ASKING 词表逐字抄了第二份——本文件 337-338 行注释自己写明「抄第二份必漂」，测试却在抄；ASKING 加词该测试不会红 | 测试直接引 `super::ASKING` | safe |
| ask.rs:226-233 | `need_intent_reply` 返回 `anyhow::Result` 但函数体无任何 `?`/fallible 操作，恒 `Ok` | 返回类型收窄为 `Option<AskResult>`，同步改 run.rs:295 调用点；接口变化需测试 | test |

## KbMindmap.vue（39 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| KbMindmap.vue:304-321,272 | `regenerate` 成功路径 bug：`await load()` 内 `++mindmapEpoch`(272)，finally 里 `epoch === mindmapEpoch` 永假 → `regenerating` 永久 true，「重新生成」按钮变砖（且 load() 不复位该标志，无任何自愈） | reload 前先复位 `regenerating`，或 finally 无条件复位 | test |
| KbMindmap.vue:213-243 | toggleDoc 的 sections 请求无 epoch 守卫：拉取途中换空间/卸载后，230 行仍把旧空间章节写入新状态（docId 撞上新树即嫁接错误数据），232 行也会污染新空间 note | 捕获 `const epoch = mindmapEpoch`，写回前判 `epoch === mindmapEpoch` | test |
| KbMindmap.vue:143,475 | key=名称路径：同一父节点下两个同名子节点产生重复 key（475 行 `:key` 冲突）且互相串折叠状态 | key 拼兄弟序号（如 `${path}/${name}#${index}`） | test |
| KbMindmap.vue:143,173,259 | 节点名含 `/` 时，`lastIndexOf('/')` 求父路径错位 → 边接错父节点、折叠键错乱 | visit 时直接记录父 key（不靠字符串切分） | test |
| KbMindmap.vue:231-235 | sections 任何失败（401/500/断网）都报「章节展开接口尚未上线」——401 时已 emit auth-expired，文案误导 | 仅 404 用该文案，其余报「章节读取失败，请稍后重试」 | safe |
| KbMindmap.vue:295-297,461-463 | load 失败（含 401）→「知识导图暂不可用…接口上线后会自动展示」：既混淆鉴权失败，又无自动重试机制，「自动展示」不成立 | 区分 401（交给 auth-expired，不置 unavailable）；文案改「刷新后展示」或加重试 | safe |
| KbMindmap.vue:283-286 + 462-463 | 未选空间也走 unavailable，文案却是「服务端导图接口尚未就绪」 | 按 spaceId 是否存在区分文案 | safe |
| KbMindmap.vue:302-322 | regenerate 成功后零反馈：load() 静默换树，若新旧树相似用户以为没生效 | 成功后 `note.value = '导图已重新生成。'` | safe |
| KbMindmap.vue:313-319 vs KbGraph.vue:622-626 | 图谱运营接口会透传服务端 `{error}` 文案（opsError），导图 regenerate 一律「接口暂不可用」——两处运营操作错误处理不一致 | 复用 opsError 同款逻辑 | safe |
| KbMindmap.vue:93,131,483-486 | labelWidth 上限 240px 参与列宽计算，但 SVG `<text>` 不截断：超长名称溢出本列、压到下一列节点 | 渲染时按等比字数截断加 `…`，并加 `<title>` 兜底全名 | safe |
| KbMindmap.vue:107-109 | badgeWidth 3+ 位固定 30px：docCount≥1000 时数字溢出胶囊 | ≥1000 显示 `999+` 或按位数继续加宽 | safe |
| KbMindmap.vue:96-104,151-159 | layout 里每个节点都 countDocs/countDescendants 递归子树，整树 O(N²)；大树下每次折叠都重算 | visit 一遍时自底向上带回子树计数（单趟 O(N)） | safe |
| KbMindmap.vue:222 | 一个文档章节加载中时点另一个文档，`if (loadingDoc.value) return` 静默吞掉点击，无任何反馈 | note 提示「正在读取另一文档的章节」或排队 | safe |
| KbMindmap.vue:258-264,480 | chunkCount=0 的章节节点：点击无任何响应，但 aria-label 落到「折叠 X」分支，且 477 行不是 leaf 类——语义、样式、行为三处矛盾 | aria/样式按「无内容」处理，或 normalizeSections 过滤 0 块章节 | safe |
| KbMindmap.vue:441,452 vs KbGraph.vue:826,831 | 「导出中」「生成中」无省略号，图谱侧「清空中…」「修复中…」有 | 统一省略号风格 | safe |
| KbMindmap.vue:347 vs KbGraph.vue:19 | 导出 SVG 字族缺 `'Microsoft YaHei UI'` 与 `system-ui`，与图谱 FONT_FAMILY（自称与 theme.css 对齐）不一致 | 抽共享常量或补齐 | safe |
| KbMindmap.vue:353 vs 473 | 贝塞尔控制点偏移 44 在导出与模板各写一份，改一处忘另一处即失真 | 提常量 `EDGE_CP = 44` | safe |
| KbMindmap.vue:356-373 vs 477,494-500,551-552 | 导出物与屏幕不一致：漏了章节点更小半径（r=3.5)、文档点虚线描边、chunkCount 章节徽标 | buildExportSvg 补齐这三处 | safe |
| KbMindmap.vue:378-385 | `<a>` 未挂 DOM 直接 click：老 Firefox 对 detached 锚点 download 不生效 | appendChild→click→remove | safe |
| KbMindmap.vue:389,413 | `props.spaceId ?? 'space'`：spaceId 为空字符串时漏网，得到「知识导图-.svg」；spaceId 含 `/\` 等字符也不过滤 | ` |  |
| KbMindmap.vue:426 vs KbGraph.vue:782-787 | 导图在 setup 顶层 `void load()`，图谱在 onMounted 里 load——同屏两组件两种首载写法 | 统一为 onMounted | safe |
| KbMindmap.vue:464 vs 452 | 空态按钮叫「生成导图」，工具栏叫「重新生成」，同一动作两个名字 | 统一（空态也可用「重新生成」） | safe |
| KbMindmap.vue:468 | `role="tree"` 但子节点无 role="treeitem"/aria-expanded——语义残缺比没有更糟（读屏报空树） | 补全 treeitem/group/aria-expanded，或降级 role="img" | safe |
| KbMindmap.vue:476-486 | 圆点 role="button" 但无 tabindex、无键盘事件，键盘用户完全无法操作导图 | 加 tabindex="0" + Enter/Space 触发 onNodeClick | test |
| KbMindmap.vue:480 | 文档节点 aria-label 恒为「展开文档 X 的章节」——已展开时仍说「展开」 | 按 expandedDocs 切换「展开/收起」 | safe |
| KbMindmap.vue:507-516 | role="dialog" 但 Esc 不能关、焦点不进不出；图谱详情卡同样无 Esc（KbGraph 888） | 容器加 @keydown.esc 关闭，打开时焦点移入 | safe |
| KbMindmap.vue:555 + 537 | mm-card `position:absolute` 在 `overflow:auto` 的滚动容器内：横向滚动导图时卡片跟着滚走 | 改 sticky，或挂到容器外的相对定位层 | safe |
| KbMindmap.vue:510 vs KbGraph.vue:862,888 | 关闭按钮只有 aria-label 没有 title；图谱两个关闭按钮都有 title——鼠标用户无悬浮提示 | 补 `title="关闭摘要"` | safe |
| KbMindmap.vue:513 | `v-if="activeSection.page"` 真值判断：page=0（若服务端 0 起页）被吞 | 改 `activeSection.page != null` | safe |
| KbMindmap.vue:552 | `.mm-dot.section { r: 3.5 }` 用 CSS 几何属性，老 Safari 不支持时静默退回 4.5 | 直接在模板 `:r` 绑定（与导出一致） | safe |
| KbMindmap.vue:555 vs KbGraph.vue:966,991 | 卡片宽固定 290px；图谱浮层用 `min(320px, calc(100% - 20px))` 自适应窄屏——同款浮层两种写法，窄屏下图定宽可能溢出 | 改用 min(290px, calc(100% - 24px)) | safe |
| KbMindmap.vue:557 vs KbGraph.vue:969,993 | 导图卡片 shadow-lg，图谱浮层 shadow-md——同级浮层阴影不统一 | 统一阴影档位 | safe |
| KbMindmap.vue:20 vs KbGraph.vue:17 | 两文件 PALETTE 近似却不同（首色 #e0a43c vs #f0a63c，8 色 vs 10 色）——同屏视觉语言漂移 | 抽共享调色板常量 | safe |
| KbMindmap.vue:538 vs KbGraph.vue:939 | 图谱画布底 --bg-main，导图画布底 --bg-card——同屏两个「画布」底色不一（若非刻意） | 统一底色变量 | safe |
| KbMindmap.vue:535 + 239 | 「没有可展开的章节结构」是中性提示，也用 warning 黄底样式 | note 支持 info/success 样式分级 | safe |
| KbMindmap.vue:41-59 | 折叠键按空间存 localStorage 但永不清理：删了的空间/改名分支的键永久残留 | 写入时按前缀裁剪（如只保留最近 N 个空间） | safe |
| KbMindmap.vue:540 | `svg { min-width: 100% }`：树比面板窄时 svg 被拉宽、viewBox 内容居中留白（preserveAspectRatio meet），小树显得「飘」 | 小树时去掉 min-width 或改 preserveAspectRatio="xMinYMid" | safe |
| KbMindmap.vue:476-481 | 圆点 r=4.5（直径 9px）点击目标过小，虽有标签兜底但圆点本身是主交互暗示 | 叠一个透明 r=12 的命中圆 | safe |
| KbMindmap.vue:181-186,36 | 折叠某分支时若摘要卡属于其下章节，卡片仍开着，内容对应节点已不可见 | toggle() 时若 activeSection 路径被折叠则关闭卡片 | safe |

## pages/ai-chat/ai-chat.vue（39 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| pages/ai-chat/ai-chat.vue:4 | 注释「新消息仅在贴底时自动滚动锚定」与 215 行代码不符（用户消息无条件滚底） | 注释补「用户消息除外」 | safe |
| pages/ai-chat/ai-chat.vue:5 | 聊天流 scroll-view 未隐藏滚动条，与 65 行表格 `:show-scrollbar="false"` 同页两口径 | chat-scroll 加 `:show-scrollbar="false"` | safe |
| pages/ai-chat/ai-chat.vue:24,121 | AI 段落有 `user-select`，用户气泡没有， selectable 口径不一致 | 121 行 text 加 `user-select` | safe |
| pages/ai-chat/ai-chat.vue:28-38,451 | asking 期间点澄清 chip 会走 onSend 取消分支（450 行注释自认），用户本想追问却变成取消请求 | onClarifyTap 加 `if (asking.value) return` 或禁用态 | test |
| pages/ai-chat/ai-chat.vue:65 | 表格横向滚动条隐藏且无可滑提示，用户未必发现可横滑 | 加右侧渐变遮罩或「可左右滑动」小字 | safe |
| pages/ai-chat/ai-chat.vue:103 | 渲染期对每条 citation 每次重渲染都做 `filter().join()` 字符串拼装 | 在 414-420 map 时预计算 locText 字段 | safe |
| pages/ai-chat/ai-chat.vue:146 | asking 期间输入框 disabled，等待响应时无法预打下个问题 | 输入框保持可用，仅发送钮受控 | test |
| pages/ai-chat/ai-chat.vue:147 | placeholder「输入你想问的问题」无示例，首用引导弱 | 改为带示例，如「试试：本月销售额是多少」 | safe |
| pages/ai-chat/ai-chat.vue:186 | 贴底阈值 40 为裸魔法数 | 提常量 `AT_BOTTOM_SLOP_PX = 40` | safe |
| pages/ai-chat/ai-chat.vue:181,590 | chatViewHeight 仅 onShow 量一次；键盘弹起压缩可视高后贴底判定仍用旧值 | 监听 `uni.onKeyboardHeightChange` 重测 | safe |
| pages/ai-chat/ai-chat.vue:185-186,194 | measure 失败时 chatViewHeight 恒 0 → onChatScroll 直接 return → atBottom 永为 true，用户上翻后新 AI 消息仍拽回底部 | onChatScroll 中高度为 0 时先补测一次再判 | safe |
| pages/ai-chat/ai-chat.vue:212 | `split(/\n{2,}/)` 不过滤空串，结尾/连续空行会渲染出带 14rpx margin 的空段落 | split 后 `.filter(p => p.trim())` | safe |
| pages/ai-chat/ai-chat.vue:216,239 | loading 气泡 push 也触发 persistSession，随即又被 360 行过滤，多一次无效同步落盘 | pushMessage 中 `m.loading` 时跳过 persist | safe |
| pages/ai-chat/ai-chat.vue:231-249,564-578 | onUnload 清了录音看门狗，但没清 LOADING_STAGES 三个定时器、没 abort 在途 askTask | onUnload 补 `loadingTimers.forEach(clearTimeout)` + `askTask?.abort()` | safe |
| pages/ai-chat/ai-chat.vue:244 | 注释「各阶段互斥」表述错误：阶段文案是依次覆盖而非互斥，幂等指的是 finishLoading | 改写注释为「阶段依次覆盖；finish 幂等」 | safe |
| pages/ai-chat/ai-chat.vue:267 | colTitle 对 null 列返回字符串 `"null"` 直接当表头 | null/undefined 回退 `'-'` | safe |
| pages/ai-chat/ai-chat.vue:276 | 宽字符区间 `[一-龥＀-￯]` 漏 U+3000 段中文标点（。、「」等），列宽低估致挤压 | 区间补 `\u3000-\u303f` | safe |
| pages/ai-chat/ai-chat.vue:285-337 | isNumCol / isCodeCol / measureText 对同一格重复调 formatCell（每格 3~4 次格式化） | parseResult 先预算 formatted 矩阵，三处复用 | safe |
| pages/ai-chat/ai-chat.vue:338 | total 只取 allRows.length；若后端 rows 已截断并另给 total 字段，「共 N 行」会漏报 | total 优先采用数字形态的 `t.total` | test |
| pages/ai-chat/ai-chat.vue:352-355,357 | 每次 persist 都同步 `getStorageSync('userInfo')` 取用户键 | onShow 时缓存 user key | safe |
| pages/ai-chat/ai-chat.vue:357-365 | 落盘含完整表格 rows（50 条消息 × 最多 50 行）+ SQL，易超小程序 storage 上限；超限被 catch 静默吞，历史丢失无感知 | 落盘前精简（截断表格行/去 sql），或捕获后降级重试一次 | test |
| pages/ai-chat/ai-chat.vue:371 | 恢复只过滤无 id 项，缺 role 的脏数据会渲染成空气泡白占 20rpx gap | 过滤条件加 `m.role` 校验 | safe |
| pages/ai-chat/ai-chat.vue:373 | 旧版本缓存的消息缺 paras 字段时，AI 正文整块渲染空白 | restore 时对 string text 缺 paras 的重算 | safe |
| pages/ai-chat/ai-chat.vue:375 | saved.convId 未校验类型，脏数据（对象/数字）会原样进 conv_id 请求体 | 仅接受 string，否则回退 '' | safe |
| pages/ai-chat/ai-chat.vue:405,432 | `code===0` 但 data 为空时落到 else 报「服务繁忙」，文案误导（请求其实成功） | 该分支单独兜底「未获取到结果，请换种问法」 | test |
| pages/ai-chat/ai-chat.vue:410-412 | `o.question.trim()` 算两次，且 label 回退用未 trim 的 question | 先取 `const q = o.question.trim()` 复用 | safe |
| pages/ai-chat/ai-chat.vue:419 | `c.page != null` 放行 0，显示「第0页」 | 改 `c.page > 0` 判定 | safe |
| pages/ai-chat/ai-chat.vue:426 | trust 只兼容对象形态 `d.trust.level`；后端若直接给字符串 `'review'`，推导口径提示条永不出现 | 兼容 string 形态 | test |
| pages/ai-chat/ai-chat.vue:443 | 非预期异常（如 parseResult 抛 TypeError）会把 JS 原始 message 弹进气泡 | 仅放行已知短文案，否则回退「网络异常，请稍后重试」 | test |
| pages/ai-chat/ai-chat.vue:459,507 | 「聆听中，松开结束」同一文案硬编码两处 | 提常量复用 | safe |
| pages/ai-chat/ai-chat.vue:459,940-945 | live-hint 无行数/溢出限制，长识别中间文本会把输入坞顶高 | CSS 加单行省略（`overflow:hidden;white-space:nowrap;text-overflow:ellipsis`） | safe |
| pages/ai-chat/ai-chat.vue:589 | 每次 onShow 都发 aiMe 校验，tab 来回切也重复请求 | 按 token 变化或时间间隔节流 | test |
| pages/ai-chat/ai-chat.vue:607 | tab 页用 `height:100vh`，H5/App 端含原生 tabbar 时易把输入坞压出可视区 | 改 `height:100%` 或按平台适配 | test |
| pages/ai-chat/ai-chat.vue:742-743 | 「意图澄清选项」注释贴在 `.cite-wrap` 上方，实际描述的是 771 行的 clarify 样式，注释错位 | 移到 `.clarify-wrap` 前 | safe |
| pages/ai-chat/ai-chat.vue:635-1001 | 主题色硬编码散落（#a39884×5、#eee4cf×4、#2f2b24×4、#6a5200×3 等），改色需多点同步 | 抽 `:root` CSS 变量统一 | safe |
| pages/ai-chat/ai-chat.vue:109,132-140,152-154 | sql-toggle / mic-btn / send-btn 均为 view+tap，无 aria-label/role，读屏不可达 | 加 aria-label（查看SQL/语音输入/发送·取消） | safe |
| pages/ai-chat/ai-chat.vue:215 | 用户上翻阅读时新到 AI 消息静默追加，无任何感知入口 | 非贴底时出「↓ 新消息」浮钮，点击回底 | test |
| pages/ai-chat/ai-chat.vue:50,55,78,84 | kv/表格行格均用索引做 key，同源数据刷新（恢复+重发）时可能复用错 DOM 态 | 行 key 改用 `ri`+首格内容组合 | safe |
| pages/ai-chat/ai-chat.vue:376 | restore 后 scrollToMsg 只调一次 nextTick，若表格图片等重布局晚到，锚定位置可能偏 | 二次 nextTick/setTimeout 兜底再滚一次 | safe |

## tools/kb_eval.py（39 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| tools/kb_eval.py:5-9 | 头部用法注释未列 `--cases`（66 行支持）与 `--selftest`（57/829 行支持），帮助与实现脱节 | 在头部注释补齐两个开关 | safe |
| tools/kb_eval.py:10-11 | 环境变量说明缺 `DMSAI_SETTINGS`（445 行 kb_root 实际消费它） | 头部补一行说明 | safe |
| tools/kb_eval.py:30 | 9 个模块单行导入，可读性差 | 拆成一行一个 | safe |
| tools/kb_eval.py:49 | `sys.exit(字符串)` 以退出码 1 退出且打到 stdout；参数错误语义是「门没开」却与「1=有题判红」撞车 | 打 stderr 并 exit 2（退出码语义变化需同步验证调用方） | test |
| tools/kb_eval.py:70 | `BASE` 未 `rstrip("/")`，deep_contract_eval.py:21 却做了；带尾斜杠时所有 URL 双斜杠 | 加 `.rstrip("/")` | safe |
| tools/kb_eval.py:72-73 | `SPEC["logins"]["a"]` 未经验证，题集缺 logins 键直接 KeyError traceback | validate_spec 里补 logins 必填与 a/b 双键校验 | test |
| tools/kb_eval.py:80 | `ASK_PATHS` 全局列表在 ask() 里被原地 remove（165 行），是隐性运行期状态，无注释说明 | 在定义处注明「运行期会被 ask() 收缩」 | safe |
| tools/kb_eval.py:96 | `json.loads` 可能返回 list/标量，调用方一律 `.get`（128、254、288 行）会 AttributeError | req 内统一 `if not isinstance(parsed, dict): parsed = {"data": parsed}` | test |
| tools/kb_eval.py:103-104 | 成功响应非 JSON 时返回 code=0，丢掉真实 HTTP 状态，与「连接失败」混淆 | 返回真实 code，error 里注明「响应非 JSON」 | safe |
| tools/kb_eval.py:105 | `TimeoutError` 是 `OSError` 子类，异常元组冗余 | 删 `TimeoutError` | safe |
| tools/kb_eval.py:121-124 | token 校验失败只标 invalid 不打 HTTP code，排障需手工重放 | 打印 `token 校验 HTTP {code}` | safe |
| tools/kb_eval.py:127-132 | 密码登录失败不输出服务端 error 摘要，只有哑的 `invalid` | 附 `str(j.get("error"))[:80]` | safe |
| tools/kb_eval.py:168 | 错误文案硬编码两个入口路径，与 ASK_PATHS 内容脱钩（改列表文案即过期） | 用已尝试路径动态拼 | safe |
| tools/kb_eval.py:192 | `login_name={login}` 未 URL 编码；494 行 doc_id 用了 quote、616 行用 urlencode，同款拼接三套写法 | 统一 `urlencode` | safe |
| tools/kb_eval.py:192 | `span > 1` 假定 span 为 int；服务端若返回字符串会 TypeError 终止整趟 | `isinstance(span, int) and span > 1` 守卫 | test |
| tools/kb_eval.py:212 | `u.port or 80` 对 https BASE 默认端口错（应为 443） | 按 scheme 取默认端口 | safe |
| tools/kb_eval.py:214-215 | embed 服务地址端口硬编码 127.0.0.1:8077；BASE 指远端时检查对象错误且无配置口 | 环境变量可配（如 DMSAI_EMBED_ADDR） | safe |
| tools/kb_eval.py:216 | `subprocess.run(["docker", ...])` 无 FileNotFoundError 兜底：docker 未装时依赖门自己 traceback，且 daemon 宕时误报「PG 容器未起」 | try/except OSError 报「docker 不可用」；检查 returncode 区分 daemon 宕 | safe |
| tools/kb_eval.py:216,402 | `docker ps` / `docker exec` 均无 timeout，docker 卡死则整趟评测挂死 | 加 `timeout=15`/`timeout=30` 并按缺席处理 | safe |
| tools/kb_eval.py:229 | 脚注正则把 `[^1]:` 定义行也计为引用，模型在文末列定义会虚增 refs | 负向断言排除后随 `:` 的匹配 | test |
| tools/kb_eval.py:255,306,337,383,508,528 | 错误截断长度 90/90/160/100/100 各处不一 | 抽常量（如 `_ERR_TAIL = 160`）统一 | safe |
| tools/kb_eval.py:288 | `citations` 若是 dict/str（畸形响应），318/328 行迭代 `.get` 直接崩，整趟终止 | isinstance 校验，非数组按「citations 结构非法」判红 | test |
| tools/kb_eval.py:293-295 vs 309-312 | forbid 用 `.lower()` 不区分大小写，keywords/must_any 区分大小写；同款子串判据两套口径且无注释 | 统一口径或注释说明刻意差异 | safe |
| tools/kb_eval.py:304 | `"...%d..." % code` 与全文件 f-string 风格不一 | 改 f-string | safe |
| tools/kb_eval.py:333-335 | chunk_text 200 时返回整个响应 JSON 的 dumps，chunk_keywords 可能命中元数据字段名而非正文，假绿 | 只拼接正文字段 | test |
| tools/kb_eval.py:337 | 多个 chunk 回查失败只报 `bad[0]`，其余证据丢 | 拼接全部（带总截断） | safe |
| tools/kb_eval.py:370,378 | validate_spec 只收集 fixtures 的 file 名，不校验条目是 dict、`as ∈ {a,b}`；非 dict 条目或 `as:"c"` 在 missing_fixtures/378 行 TypeError/KeyError | 预检补 fixtures 条目结构与 as 取值 | test |
| tools/kb_eval.py:381 | `if isinstance(j, dict)` 与 379-380 行的归一重复，是死条件 | 删掉后半截 | safe |
| tools/kb_eval.py:392 | `j.get('status')`/`chunk_count` 可能为 None，打印「None None 块」 | 给默认值或按需打印 | safe |
| tools/kb_eval.py:413 | pg_json 多行返回时静默取 `lines[-1]`，约定只在注释外 | 注释注明「查询约定单行 JSON」或断言单行 | safe |
| tools/kb_eval.py:452 | `json.loads(settings).get("kb_root")`：settings 顶层非 dict 时 AttributeError 不在捕获列表 | 加 isinstance 判断或补捕获 | safe |
| tools/kb_eval.py:576-603 | 停用/启用两段「搜索+PG 状态核验」近乎逐字重复，漂移风险 | 抽 `_assert_recall_and_state(item, query, want_status)` | safe |
| tools/kb_eval.py:663-700 | validate_spec 不校验 case 的 `name`/`kind`/`question` 必填；840 行 `c["name"]`、844 行 `c["kind"]`、355 行 `c["question"]` 运行期 KeyError | 预检补三项必填 | test |
| tools/kb_eval.py:671 | 未查重题名（evaluation.py:256-258 有同款检查），重名题报告无法区分 | 补题名查重 | safe |
| tools/kb_eval.py:686,690,697 | `contracts.get("metadata"/"versions"/"lifecycle")` 若题集写成 dict/str，迭代后 `.get` 崩 | 校验三键必须是数组 | test |
| tools/kb_eval.py:736 | `assert validate_spec(SPEC) == [], validate_spec(SPEC)` 同一校验跑两遍 | 存局部变量复用 | safe |
| tools/kb_eval.py:900-906 | `contract_failures` 把 rc 从 2 压成 1，且 906 行「门没开」消息只在 rc==2 才打印；契约失败+夹具阻塞时阻塞证据完全消失 | 906 行改为 `if bad:` 独立打印 | safe |
| tools/kb_eval.py:908-909 | 清理失败把 rc 从 1（题红）改写成 2（门没开），归因反转 | 只在 rc==0 时升为 2，或注释说明优先级 | test |
| tools/kb_eval.py:863-867 | 两种身份都 none/invalid 时无前置警示；非 ACL 题会全 401 记成「答错」（1），归因错误 | 打印 ⚠️「双身份均未建立，问答类题将记红而非阻塞」 | safe |

## retrieve.rs（39 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| retrieve.rs:236 | `out.trim().to_string()` 冗余：gap 逻辑保证 `out` 无首尾空白（前导分隔符 `gap=!out.is_empty()`=false 不会写入，尾随分隔符只置 flag 不推字符），trim 白分配一次 String | 改 `let mut q = out;` | safe |
| retrieve.rs:399 | 局部变量 `visible_docs` 遮蔽同名函数 `visible_docs()`（L687），阅读时易混 | 改名 `visible_n` | safe |
| retrieve.rs:439,467 | `weights.route_array()` 调用两次，构造两遍相同数组 | 函数开头 `let routes = weights.route_array();` 两处复用 | safe |
| retrieve.rs:505-525 | 零命中诊断日志结构性地恒为全零：能进此分支 ⇒ `ids` 空 ⇒ `ranked` 空 ⇒ 八路 list 全空（RRF 不过滤任何候选，阈值过滤发生在 SQL 内），注释（L498-504）承诺的②「RRF 后被阈值滤光」在此日志永远观测不到，②与③无法区分；且 `stats`（L468）已算好同名字段却又从 `lists` 重算一遍（还漏掉 relation 一路） | 直接记 `stats` 各字段（含 relation_candidates），并修正注释承认②③在此同形态 | test |
| retrieve.rs:437-438 | `auxiliary[0]` 裸下标指向 metadata 路，纯靠 push 序约定；未来在 metadata 前插一路会静默指错 | 加 `debug_assert_eq!(lists.len(), 5)` + 注释钉住「auxiliary[0] = metadata 路」 | safe |
| retrieve.rs:468-479 | `stats` 按 `lists[0..7]` 硬下标取数，同样依赖 push 序，多/少 push 一次就静默错位 | 构造前 `debug_assert_eq!(lists.len(), 8)` | safe |
| retrieve.rs:542 | 每个 hit 都 `ranked.iter().find(⋯)` 线性找分，O(hits×fused) | 先建 `HashMap<i64, f32>` 一次，循环内查表 | safe |
| retrieve.rs:543 | `match_channels` 每 hit 对 8 路 Vec 做 `contains` 并逐路分配 String；n 小但属无谓重算 | 每路预建 `HashSet<i64>`，或离线建一次 `HashMap<i64, Vec<&str>>` 复用 | safe |
| retrieve.rs:608 | `span.clamp(1, 16)` 字面量 16 与 `MAX_MERGE_SPAN`（L147）重复定义，L146 注释明说两者必须一致；改常量时两处会漂 | 改 `span.clamp(1, MAX_MERGE_SPAN)` | safe |
| retrieve.rs:582 | `w.clamp(0, 3)` 魔法数 3 只在注释（L588-590）里解释，代码无名 | 提 `const CITATION_WINDOW_MAX: i32 = 3;` | safe |
| retrieve.rs:573-581,599-605 | `window` 与 `span` 的锚点查询块逐字重复（同一 SQL、同一 bind 序列、同一 `ok_or_else`） | 提 `async fn citation_anchor(store, v, chunk_id) -> Result<(String, i32), KbError>` | safe |
| retrieve.rs:1332 | `keep_auxiliary_votes_on_direct_hits` 文档写「已由正文/标题召回的块」，实际 `direct` 是四路（向量/精确/正文/标题） | 注释改「四路直接命中（向量/精确/正文/标题）」 | safe |
| retrieve.rs:1415 | `diversify(ranked.clone(), TOP_K)` 对整表深拷贝（每个 Hit 含长 text 等多个 String），实际只需 ≤limit 条 | `diversify` 改吃 `&[Hit]`，两处 push 各自 `h.clone()`（L2378 等价性测试护着） | safe |
| retrieve.rs:1489-1493 | `preserve_governed_versions`：`governed_version_key(existing)` 在去重比较里反复重算，且 `Some(key.clone())` 每次比较都分配 String | 循环外为 `ranked` 预算 `Vec<Option<key>>`；比较用 `.as_ref() == Some(&key)` | safe |
| retrieve.rs:1554-1570 | `preserve_textual_versions`：`textual_version_class`/`textual_version_group` 在 O(n²) 双重循环里反复重算——class 要对合并后长正文做 8 个 marker `contains`，group 有 to_lowercase/replace 多次分配 | 进入循环前一次性算出 `Vec<(Option<i8>, String)>` 复用 | safe |
| retrieve.rs:1514-1515,1708-1711 | 旧版/新版 marker 列表（"旧版/历史版/历史口径/废止"、"新版/现行版/现行口径/修订版"）在 `textual_version_class` 与 `opposite_version_sections` 各硬编码一份，改一处漏一处即行为分叉 | 提两个模块级 `const` 数组共用 | safe |
| retrieve.rs:1502,1577 | `added >= 2 |  | out.len() >= limit + 2` 的魔法数 2 在两个 preserve 函数各写一遍 |
| retrieve.rs:1630-1633 | `dedup_text` 内层 `any` 闭包里对每个已收条目反复 `normalized_text(&p.text)`，O(n²) 字符串归一化+重复分配 | `out` 旁维护 `Vec<(String, String)>`（doc_id, 归一化键）缓存 | safe |
| retrieve.rs:1631 | `key.is_empty() |  | ` 豁免：归一化后为空的块（纯空白正文）不参与去重，同文档多个空白块会重复占候选位 |
| retrieve.rs:1627 | `dedup_text` 无注释说明「只在同 doc_id 内按归一化正文去重；跨文档同正文由 `dedup_sources` 负责」这一分工 | 补一句文档注释 | safe |
| retrieve.rs:1663-1664 | `merge_adjacent` 文档注释称 `heading_path` 「取首块」，但 L1683-1685 实际是「首块为空则取后续块第一个非空」（L2319 测试钉着该行为） | 注释改「`heading_path` 取组内第一个非空」 | safe |
| retrieve.rs:811-813 | `TRGM_SQL`：`word_similarity($2, text)` 在 WHERE 与 ORDER BY 各出现一次，顺序扫描下每行可能算两遍（PG 不在 qual 与排序表达式间做 CSE） | 包子查询先算 `sim`，外层过滤+排序复用（需连库验证计划不退化） | test |
| retrieve.rs:821-830 | `TITLE_SQL`：同一个 `GREATEST(word_similarity(...), word_similarity(...))` 在 SELECT 与 WHERE 各算一遍（每 chunk 行 2× 双 word_similarity） | 内层子查询先算 `sim`、外层 `WHERE sim > $3`（过滤先于 DISTINCT ON，语义不变） | test |
| retrieve.rs:840-861 | `METADATA_SQL`：`sim`（8 项 GREATEST）只依赖 doc 级字段，却在 doc×chunk JOIN 结果上逐行算——每篇文档的元数据相似度被重复算「其块数」遍 | 先在 `kb.doc` 子查询按 doc 算一次 `sim`，再 JOIN chunk 选代表块 | test |
| retrieve.rs:1072-1077 | 种子并集未再用 `KG_SEED_MAX` 收口：`by_chunk`、`by_name` 各自 ≤20，并集可达 40，与 L125-126「种子实体数上限：个性化压在少数实体上」口径不符 | 合并去重后 `seeds.truncate(KG_SEED_MAX)` | test |
| retrieve.rs:1105-1106 | hop1 边在 `rel_edges` 里被重复计入：`frontier` 含 seeds，hop2 查询结果覆盖 hop1 边（L1126 注释只说了实体权重去重），`assemble_subgraph` 的 `+= e.weight`（L1193）使 hop1 关系边权重翻倍、hop1-hop1/hop1-hop2 边不翻倍，是否故意无注释覆盖 | 确认是否有意；无意则 `rel_edges` 按 (src,dst) 去重，有意则补注释钉住 | test |
| retrieve.rs:1113-1123 | `new_endpoints`：`known.contains` + `out.contains` 双重线性查找，known≤200、端点≤2000 时数十万次字符串比较 | known/out 各旁挂 `HashSet<&str>` 判重，Vec 仍保序 | safe |
| retrieve.rs:1141-1153 | `diffuse_entities`：`out.iter().any(⋯)` 线性查重，上限 200 → 约 2 万次比较 | 旁挂 `HashSet<&str>` | safe |
| retrieve.rs:1141 | hop2 权重 `0.0` 是裸字面量，而 1.0/0.8 都有具名 const（`KG_SEED_DIRECT`/`KG_SEED_NEIGHBOR`） | 提 `const KG_SEED_HOP2: f64 = 0.0;` | safe |
| retrieve.rs:1232 | PPR：dangling 节点集合由 `out_w` 决定、迭代期间不变，却每轮 `(0..n).filter(out_w[i]==0.0)` 重算 | 循环外算一次 `Vec<usize>`（dangling 下标）复用 | safe |
| retrieve.rs:1237-1238 | PPR：每条边每轮做 `w / out_w[a]`、`w / out_w[b]` 除法，商在迭代期间不变 | 循环外预归一化为有向弧 (from,to,w/out_w[from])（除法确定性，结果逐位相同） | safe |
| retrieve.rs:1233 | PPR：`next` 每轮重新 `Vec::collect` 分配（≤100 轮） | 循环外分配双 buffer 轮换、迭代末清零 swap | safe |
| retrieve.rs:1309 | `source_uri: Some(record.source_uri.clone())`：远程记录 source_uri 为空串时产出 `Some("")`，「来源可辨」变成空链接 | `Some(...).filter( | s |
| retrieve.rs:416-418 | 向量降级 warn 无 `space` 字段：多空间部署下无法定位哪次检索降级 | 加 `space = space.unwrap_or("<all>")` 结构化字段 | safe |
| retrieve.rs:514-524 | 零命中 info 日志同样无 `space` 字段，跨空间排查要靠时间戳对齐 | 加 `space` 字段 | safe |
| retrieve.rs:380 | `search_report` 文档注释「只额外返回各召回路线的候选数量」——实际还带回 `normalized_query` 与 `vector_degraded` | 改注释如实描述 | safe |
| retrieve.rs:1371-1374 | `rrf` 文档写「score = Σ 1/(60 + rank)」漏权重因子（`rrf_weighted` 实际是 Σ w/(60+rank)，`rrf` 只是 test 壳） | 注释补权重说明或挪到 `rrf_weighted` 上 | safe |
| retrieve.rs:1322-1326 | `match_channels` 的 `NAMES` 顺序与 lists 槽位序（L436/455/456/465）纯靠约定对齐 | 函数入口 `debug_assert_eq!(lists.len(), NAMES.len())` | safe |
| retrieve.rs:776 | token 门槛 `cur.len() >= 6` 的 6 是裸字面量，L757/L769 注释各引一次 `{6,}` | 提 `const EXACT_TOKEN_MIN: usize = 6;` | safe |

## crates/server/src/datamap_api.rs（38 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/server/src/datamap_api.rs:15-17,158-163 | 纪律 4 声称「所有 SQL 走 `OwnedStore::fixed`」，但 `migrate()` 用 `sqlx::query(stmt)` 直连（`stmt` 是切分产物，编译期不是 `&'static`），头注与代码不符 | 头注收窄为「除 migrate 外全走 fixed」，或 migrate 改收 `&OwnedStore` 走 fixed 通道 | safe |
| crates/server/src/datamap_api.rs:157-163 | 两个 for 循环逐字重复（split/trim/filter/execute） | 抽 `async fn run_stmts(pg, text)` 小助手，两调用点各一行 | safe |
| crates/server/src/datamap_api.rs:159,162 | 语句执行失败时 `?` 直接透出，丢失「是哪一句挂了」——DDL 五句、WIDEN 两句，报错无法定位 | `map_err` 附语句序号与前 ~60 字摘要再 `?` | safe |
| crates/server/src/datamap_api.rs:149-152,161-163 | `KIND_CHECK_WIDEN` 每次启动无条件 DROP+ADD，即使约束已是七值；ALTER TABLE 拿 ACCESS EXCLUSIVE 锁，启动即锁表两次 | 先 `SELECT pg_constraint` 探测约束文本，已是目标形态就跳过 | test |
| crates/server/src/datamap_api.rs:150-151,161-163 | DROP 与 ADD 不在同一事务：进程在两句之间被杀，表在下一次启动前处于无 kind CHECK 窗口，可写入非法 kind | 包进一个事务，或合并成单条 `ALTER TABLE … DROP CONSTRAINT IF EXISTS …, ADD CONSTRAINT …` | test |
| crates/server/src/datamap_api.rs:184 | `LIMIT 500`（复核队列契约）无任何测试钉着——ds 谓词有 `ds_predicates_are_inlined` 守卫，LIMIT 没有；头注 L42 的「500」是第二份无守卫事实 | 补一条断言 `INFERRED_EDGES_SQL.contains("LIMIT 500")` 的漂移守卫（照 L1293 模子） | safe |
| crates/server/src/datamap_api.rs:96-99 | `internal_err` 的 warn 只有 context+error，不带 ds/login/路由字段；11 个调用点（489/555/654/670/746/774/822/842/876/971/1025）排障时定位不到是哪个 ds | 调用点补 `ds = %ds` 等结构化字段，或给 `internal_err` 加 kv 参数 | safe |
| crates/server/src/datamap_api.rs:97 | 500 级内部错误用 `warn!` 而非 `error!`——按 error 级做告警的运维通道会漏掉全部真实 500 | 升 `error!`；若刻意对齐 kb_api 模子则在注释写明理由 | safe |
| crates/server/src/datamap_api.rs:103-106 | `identity_err` 把业务性拒绝（用户不存在/多角色未选）与身份库故障（连接超时）混成同级 warn；未认证探测每次 403 刷一条 warn，日志泛洪且淹没真故障 | 按 anyhow 内容分类：业务拒绝降 `info!`，基础设施错误保留 `warn!` | test |
| crates/server/src/datamap_api.rs:243-244 | `rsplit('.').next()` 对 `&str` 永不返回 `None`（空串也产一项），`unwrap_or_default()` 是死防御分支 | 直接 `.unwrap_or("")` 语义相同，或注释注明「rsplit 恒产一项」 | safe |
| crates/server/src/datamap_api.rs:263-264,322,711-712 | from/to 的 `bare_table` 归一被重复计算 3 次（shortest_path 内、path_nodes 起点、paths_result_json 输出），每次 1-2 个 String 分配 | 归一一次后把裸名透传给三处 | safe |
| crates/server/src/datamap_api.rs:272-282 | 邻接表 `HashMap<String, Vec<(String, usize, bool)>>` 把每条边的两个表名各克隆 2 次；可用「名称 Vec + 节点下标」省掉全部字符串克隆 | 微重构为 u32 下标邻接（500 边护栏内收益小，纯可读性/分配优化） | safe |
| crates/server/src/datamap_api.rs:294-295 | BFS 对每个未访问邻居都 `path.clone()`（一次 Vec 分配），命中目标的分支也先克隆再返回 | 命中分支直接复用 `path`，仅入队分支克隆 | safe |
| crates/server/src/datamap_api.rs:348-353,795,866 | `review_transition` 的 `Ok(&'static str)` 两个调用点都不消费（只判 Err）；`ReviewAction::target`（L337-343）仅为该 Ok 值存在 | 改返 `Result<(), String>`，target 并入或删除 | test |
| crates/server/src/datamap_api.rs:798-799,359-363 | accept 先 `matches!(kind, "join"\ | "joinable")` 再调 `joinable()` 把 kind 重查一遍——joinable 的 kind 分支在唯一生产调用点是死判定 | 调用点只查双列非空，或 joinable 拆成 kind 判定 + 列判定两函数 |
| crates/server/src/datamap_api.rs:373-403,594-595 | `edge_status_filter`/`edge_kind_filter` 每次请求 `to_string()` 分配 `Vec<String>`（缺省 kind 分支一次 7 个 String），值全是字面量 | 返回 `&'static [&'static str]`，`load_inferred_edges` 签名改 `&[&str]` | test |
| crates/server/src/datamap_api.rs:429,549 | `s.len() <= 64` 是字节数，错误文案说「≤64 字符」——ASCII 白名单使两者等价但单位表述不一致 | 在 valid_ds 注释注明「ASCII 限定故字节数=字符数」 | safe |
| crates/server/src/datamap_api.rs:476 | `&[p.role_code.clone()]` 为造一个单元素切片克隆 String 并分配临时 Vec | `std::slice::from_ref(&p.role_code)`，零分配同语义 | safe |
| crates/server/src/datamap_api.rs:471-478 | `ds_visible` 每次请求全量重算可见 ds 列表（一次 PG 往返）；同一 (login, role) 秒级窗口内结果不变，5 个端点每调用都付一次 | 可评估短 TTL 缓存（注意 ACL 变更的延迟放行风险，需安全评审） | test |
| crates/server/src/datamap_api.rs:491 | 「无权访问数据源 {ds}」对不可见 ds 证实了其存在性——ds 名枚举 oracle | 文案改「数据源不存在或无权访问」 | test |
| crates/server/src/datamap_api.rs:511-512 | tables/columns 两条互不依赖的查询串行 await，白付一次 PG RTT | `tokio::join!` 并发两条 fetch | test |
| crates/server/src/datamap_api.rs:648-671 | edges 端点注册表查询（L648-654）与推断边查询（L667-671）同样串行、互不依赖 | 同上 `tokio::join!` | test |
| crates/server/src/datamap_api.rs:511-538 | `load_nodes` 无总量护栏：column_doc 上万列时响应无界——paths 有 500 边 422（L747）、推断边有 LIMIT 500（L184），独缺 nodes | 超阈值时 warn 留痕，或对齐 paths 风格加 422/截断护栏 | test |
| crates/server/src/datamap_api.rs:505-506,570-585,766 | TableRow/ColumnRow/RegistryEdgeRow/InferredEdgeRow/EdgeRow 全靠元组位置对齐 SELECT 列序，列序漂移要等 decode 错才以 500 炸出；AuditRow（L922-945）已示范按名 `try_get` 的自检模式 | 至少把 12 元组 `InferredEdgeRow` 改命名 struct + 手写 FromRow | test |
| crates/server/src/datamap_api.rs:189,863 | reject 只需 status，却经 `EDGE_BY_ID_SQL` 把 evidence（长度无界的 text）等 5 列全捞回再丢弃 4 个 | reject 用只 `SELECT status` 的轻量字面量（SELECT 不受唯一写入口测试约束） | test |
| crates/server/src/datamap_api.rs:807-810 | 默认 note 先把完整 `evidence`（长度无界）`format!` 进内存再 `clip_note` 到 500 字，大 evidence 白分配 | 拼固定前缀后对 evidence 取 `chars().take(500 − 前缀字符数)`，结果逐字相同 | safe |
| crates/server/src/datamap_api.rs:197,211,215 | 三句复核 UPDATE 都不刷 `updated_at`，而其余所有写入方都刷（semantic/datamap.rs:832、lineage.rs:462）——复核后 `updated_at` 停留在上次 upsert，只剩 `reviewed_at` 是对的，列语义漂移 | 三句 UPDATE 各加 `updated_at = now()`（CAS 测试按文本断言不受影响） | test |
| crates/server/src/datamap_api.rs:953-961 | audit_sql 先做 `caller`（一次 MySQL 身份库往返）再校验 status/limit；同模块 nodes/edges/paths/relations 全是先便宜校验后身份——畸形请求白打身份库 | status/limit 校验移到 `caller` 之前（未认证+畸形请求的 401/400 优先级随之变化） | test |
| crates/server/src/datamap_api.rs:729-731,265-267 | from/to 只查原始非空；`"."`、`` "` `" `` 这类归一后为空的输入落进 `shortest_path` 返 None → `found=false`(200)，把「坏输入」与「不连通」混为一谈——恰是 L48-49 头注反对的混淆 | 归一后为空 → 400（与 L729 校验合并） | test |
| crates/server/src/datamap_api.rs:977-978 | 每行 `context_summary` 都走 `serde_json::from_str`，老行空串必走一遍解析失败路径（×最多 500 行） | `if r.context_summary.is_empty() { Null } else { from_str … }` 短路 | safe |
| crates/server/src/datamap_api.rs:977-978 | 解析失败静默 `unwrap_or(Null)` 无留痕——写侧 bug 产出的坏 JSON 会被永久吞掉、无人察觉 | 失败分支 `tracing::debug!(id = r.id, …)` 留一条 | safe |
| crates/server/src/datamap_api.rs:690-719 | `paths_result_json` 是声明为「纯函数可测」的组装层，却没有任何直接单测——`found=false` 时 nodes=[]/path=[]/edges_considered 的 JSON 形状零覆盖 | 补 found=true/false 两条形状断言 | safe |
| crates/server/src/datamap_api.rs:1389 | `src.split("#[cfg(test)]").next().unwrap_or("")`：若 `#[cfg(test)]` 属性被改名/删除，`unwrap_or("")` 分支让源码守卫 vacuous 通过（空串不含任何坏模式） | 改 `.expect("…")` 并断言 `!code.is_empty()` | safe |
| crates/server/src/datamap_api.rs:784-791,798 | 非 join 的 accept（lineage 等）：card 先过校验（L786-791）随后被静默丢弃、不落任何列——与本模块「闭集不静默忽略」的既定风格（L41-42）不一致 | 非 join 且 card 非空 → 400，或在头注 L53 写明「非 join 忽略 card」 | test |
| crates/server/src/datamap_api.rs:27,35,45,62,68 | 头注把 `login_name=&role_code=` 写成常规入参，但缺省配置下 Bearer/API key 才是唯一身份通道（main.rs:1733-1736，`insecure_login_fallback` 默认关）——照头注集成的调用方只会拿 401 | 头注补一句「login_name 回退需显式开启 insecure_login_fallback」 | safe |
| crates/server/src/datamap_api.rs:15-17,744 | `paths` 走 `load_join_edges`，其 SQL 是 `format!` 动态拼（semantic/registry/model.rs:221-229）——纪律 4「所有 SQL 走 fixed」的实际例外，头注未声明 | 纪律 4 注明该例外及理由（运行期 ds 谓词拼不进 `'static`） | safe |
| crates/server/src/datamap_api.rs:139-140,183-184 | `idx_datamap_edge_ds(ds_id, status)` 不覆盖复核队列主查询的 `kind = ANY + ORDER BY confidence DESC`——头注自述「一轮上万条」，每次②查询都要过滤后全排 | 评估 `(ds_id, status, kind, confidence DESC)` 复合索引；不建则在 L139 注释写明量级理由 | test |
| crates/server/src/datamap_api.rs:1334-1339 | `extract` 闭包对缺失文本返回 None，`assert_eq!(extract(DDL), extract(stmts[1]))` 在两侧同时缺失时 vacuous 通过（L1312 虽另守 DDL 侧，WIDEN 侧变形仍可能双 None） | 先 `assert!(extract(DDL).is_some())` 再比较 | safe |

## ingest.rs（38 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| ingest.rs:111-116 | 超限文案用整除 MB：max_bytes 非 MB 对齐或 len<1MB 时会出现「文件 0 MB 超过上限 0 MB」式误导 | 改用向上取整或保留一位小数 | safe |
| ingest.rs:120-124 | 「支持的类型」文案是手抄清单，已与 EXTS 漂移（`markdown` 在表白名单里、文案里没有），下次加格式还会再漂 | 从 EXTS 常量生成文案 | safe |
| ingest.rs:118-136 | 拒绝路径上 `ext_of` 算两次（lookup 一次、文案一次），且每次分配小写 String；lookup 内可用 `eq_ignore_ascii_case` 避免分配 | ext 提取一次下传；`EXTS.iter().find( | (e,_) |
| ingest.rs:237/516 | `classify` 已查出 FileKind 却丢弃，`parse_input` 再 `lookup` 一次重判 is_image | classify 结果随 req 传递或存局部变量 | safe |
| ingest.rs:163-190 | `infer_doc_version` 对每个 char 位置都跑 `unwrap_version_token`+`is_version_token`，长文件名近 O(n²) | 只在分隔符/边界候选位置尝试（等价收敛） | safe |
| ingest.rs:205 | `is_version_token` 每次分配 `Vec<char>` 仅为按下标判断 | 用 `chars().next()`/迭代器改写，零分配 | safe |
| ingest.rs:212 | `trim_start_matches('第')` 会剥掉多个「第」，`第第3版` 被误判为版本尾缀 | 改为 `strip_prefix('第')` + `strip_suffix('版')` 各一次 | test |
| ingest.rs:181 | 120/60 两个长度上限是裸魔法数 | 提常量并注释来源 | safe |
| ingest.rs:269-282 与 363-376 | 版本推断+warn 块原样重复两份 | 抽 `try_apply_inferred_version(st,v,doc_id,file_name)` 小函数 | safe |
| ingest.rs:280/374 | `is_err()` 吞掉错误本体，warn 里没有 error 字段，排障拿不到原因 | `.map_err( | e |
| ingest.rs:287 | 失败文案落库的 `set_status` 用 `let _` 静默——落库也失败时用户和运维都看不见 | 失败时补 `tracing::warn!` | safe |
| ingest.rs:307-311 | dedup 后 `get_doc` 为 None（并发删除）会走 Reprocess，最终报「写权限已失效」，语义误导 | `row` 为 None 时直接 `KbError::NotFound` | test |
| ingest.rs:344 | staged 临时文件删除结果 `let _` 丢弃；删除失败会静默遗留 stage 文件 | 失败时 `tracing::warn!`（不中断主流程） | safe |
| ingest.rs:402/344 | `doc_path(cfg, stage_id, ..)` 在 build_shadow 内外各算一次，漂移风险 | build_shadow 返回路径或调用方算一次传入 | safe |
| ingest.rs:403/450 | `create_dir_all(&cfg.root)` 在 build_shadow 与 run 各调一次 | 入口（ingest/reprocess）统一建一次 | safe |
| ingest.rs:418-422 | build_shadow 里向量条数不符与「服务不可用」共用一句文案，且无计数日志（run 的 482-484 有 warn，这里没有） | 不符时 warn 出 chunks/vecs 计数，文案区分两类失败 | safe |
| ingest.rs:472 | `texts` 对全部 chunk 文本 clone 一遍（大文档 MB 级），仅为满足 `&[String]` 签名 | `embed_passages` 改收 `&[&str]`（跨 crate 签名变更） | test |
| ingest.rs:482 | warn 里把条数命名为 `vecs`（实为 `v.len()`），与外层向量语义同名易误读 | 改名 `got_vecs`/`got_len` | safe |
| ingest.rs:522/557 | `usable_image_ocr` 精确匹配两个串，`[无法辨认]。`、全角括号等变体会被当成有效 OCR 正文 | 判定改「去标点空白后等于无法辨认」或归一化比较 | test |
| ingest.rs:545 | `NotFound("待解析文件")` 不带路径/文件名，运维定位困难 | 文案带 `path`（或 file_name） | safe |
| ingest.rs:546 | 兜底分支丢弃 DocError 本体且无日志，「文档处理失败」无法回溯上游原因 | 映射前 `tracing::warn!(error=%e)` | safe |
| ingest.rs:603 | 建表失败的 `{e}` 原文进用户可见 notice，与 537-548「上游正文不进用户字段」的 sanitize 纪律不一致（取决于 tabular 错误是否已净化） | 确认/复用 sanitize 后再进 notice | test |
| ingest.rs:594/602/604 | `drop_source`/`append_notice` 失败全部 `let _` 静默——孤儿物理表、丢失降级提示均无痕迹 | 各补 `tracing::warn!` | safe |
| ingest.rs:341+416 | reprocess 用**请求侧** file_name/folder_path 预计算 embedding_text，而 `replace_chunks` SQL（store.rs:1367-1369）用**库内** d.name/d.folder_path 做 expected CAS；改名/换目录后重传同内容文件 → 全部 expected 失配 → 向量全 NULL 走补算 | build_shadow 改用库内 DocRow 的 name/folder_path 生成 expected | test |
| ingest.rs:653-655 | `requested_preset` 是一行透传包装 | 内联 `req.preset`（保留 doc 注释于调用点） | safe |
| ingest.rs:658-660 vs store.rs:1171-1173 | `est_tokens` 同口径实现两份，各自声称与 Python 对齐，漂移无人拦 | 一份 `pub(crate)` 共享 | safe |
| ingest.rs:749-757 vs store.rs:1195-1199 | `one_page`/`merged_page` 同纪律两份实现 | 共享一个函数 | safe |
| ingest.rs:926-935 | `stream_lines` 每行 clone 一次 heading（大文档大量重复分配） | StreamLine 存 block 下标，heading 用时再取 | safe |
| ingest.rs:953 | `law_marker` 为 starts_with 分配整个 `after: String` | 用 char_indices 取字节偏移后切片 | safe |
| ingest.rs:1024 | ` |  | ` 这类行通过 starts/ends/matches 检查，产出 `[""]` 被 qa 主循环当表格行 |
| ingest.rs:1117/1120/1126 | qa 主循环对同一行 `parse_md_row` 两次（cells 一次、is_md_separator_row 内部一次），下一行判表头时又解析一次 | `is_md_separator_row` 改收已解析的 cells | safe |
| ingest.rs:1042/1130/1132 | `strip_prefix_ci` 整行 `to_ascii_lowercase` 分配且每行调两次；表头识别每个 cell 又分配一次小写串 | 前缀比较用 `t.get(..w.len()).is_some_and( | p |
| ingest.rs:1073 | `seen` 用 Vec 线性去重，QA 对多时 O(n²) | 换 `HashSet<(String,String)>` | safe |
| ingest.rs:1116 | 每行 `trim().to_string()` 分配，仅用于切片解析 | 保留 `&str`，改动下游签名 | safe |
| ingest.rs:1214 | `find_from` 末尾 `needle.chars().count()` 全串重扫 | 用 `s.char_of_byte(byte_from + rel + needle.len())` 得 end | safe |
| ingest.rs:1266-1281 | `sheet_block` 每行一次 `format!`+`push_str` 重复分配 | 用 `write!`/`push_str` 拼接或预分配容量 | safe |
| ingest.rs:1271-1272 | sheet 单元格含 ` | `/`\n` 未转义，会破坏 markdown 表结构并干扰 qa 表格抽取 | 渲染时转义 `\ |
| ingest.rs:1275-1276 | 注释硬编码「前 500 行」，SHEET_ROWS 改值后注释即腐化 | 注释改为引用常量名 | safe |

## KbEval.vue（37 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| KbEval.vue:42 | `headers()` 抛出的「登录会话已失效」消息在调用处全部被 catch 吞掉，用户实际看到的是「评估接口暂不可用」，该文案从未展示 | 要么删除 throw 的消息、要么 catch 时优先展示该消息 | safe |
| KbEval.vue:57-61 | `num()` 对空字符串返回 0（`Number('')===0`），后端若回 `""` 字段会被当成 0 而非缺失 | 过滤 `value === ''` 再 Number | test |
| KbEval.vue:116,413 | `[1, 3, 5, 10]` 召回档位数组两处硬编码，且 413 行每次渲染每行都新建数组字面量 | 提升为模块级常量 `RECALL_KS`，两处复用 | safe |
| KbEval.vue:123 vs 135 | `isRunning` 匹配「进行中」，`statusText` 匹配更宽的「进行」：状态为「进行」时显示「运行中」却不触发轮询 | 两处正则统一为同一判定 | test |
| KbEval.vue:132-136 | `statusText` 先判 completed 再判 failed，`completed_with_errors` 类状态会显示「已完成」掩盖失败 | failed 判定提前 | test |
| KbEval.vue:145-154 | `durationText` 对 <1 秒显示「0秒」，对负值（异常数据）也显示「0秒」 | <1000ms 显示「<1秒」，负值归一为 '-' | safe |
| KbEval.vue:159-161 | `Intl.DateTimeFormat` 每行每次渲染都新建实例 | 提升为模块级单例 formatter | safe |
| KbEval.vue:155-162 | `dateText` 不显示年份，跨年评估记录无法区分 | 非当年时补年份 | safe |
| KbEval.vue:186 | `response.ok` 但返回非 JSON 时 `data={}` → 静默显示「还没有评估记录」，掩盖接口异常 | JSON 解析失败时按不可用处理 | test |
| KbEval.vue:197-200 | `clearTimeout(pollTimer)` 在 epoch 守卫之外：旧 epoch 的迟到响应会清掉新 epoch 刚排好的轮询定时器，轮询静默中断（openReport 267-270 同样问题） | clearTimeout 移入 `epoch === evalEpoch` 分支内 | test |
| KbEval.vue:199,269,355 | 轮询间隔 5000ms 两处硬编码，且 355 行文案「每 5 秒自动刷新」第三处手写，改一处即失真 | 抽 `POLL_MS = 5000` 常量，文案插值引用 | safe |
| KbEval.vue:211 | `size > 0` 判断在 `Math.floor` 之前：输入 0.5 会发出 `sample_size: 0`；`max="500"` 也未在代码中钳制 | `Math.floor` 后判 `>= 1` 并 `Math.min(500, …)` | safe |
| KbEval.vue:219 | `data.error` 可能是非字符串（对象），`new Error(obj)` 消息变 `[object Object]` | `String(data.error ?? …)` 包裹 | safe |
| KbEval.vue:226 | 创建失败一律提示「评估接口暂不可用，创建未生效」，吞掉服务端返回的具体错误（如校验失败）；且网络错误时 run 可能已在服务端创建，「未生效」不准确 | 优先展示 `data.error`，措辞改为「创建失败」类中性文案 | safe |
| KbEval.vue:251 | `sum = {...summary, ...report}` 顶层覆盖 summary：若顶层带 `score: null` 而 summary 里有有效值，null 胜出显示 '-' | 覆盖时跳过 null/undefined，或反转合并方向后按需取值 | test |
| KbEval.vue:263 | 静默轮询失败时不设置任何标记，页面停在旧数据且无任何「刷新失败」提示 | 静默失败时给一个小号「刷新失败，重试中」提示 | safe |
| KbEval.vue:277 | `backToList` 用非 silent `loadRuns`，返回列表时闪一次全屏加载态（数据通常很新） | 改为 `loadRuns(evalEpoch, true)` | safe |
| KbEval.vue:289-292 | 无 spaceId 时复用「评估功能暂不可用/服务端评估接口尚未就绪」文案，与真实原因（未选空间）不符 | 单独分支文案「请先选择知识空间」 | safe |
| KbEval.vue:296 | 只 watch `spaceId`，token 失效后重新登录（spaceId 没变）不会重载，停在「暂不可用」 | 同时 watch `props.token` | test |
| KbEval.vue:316-317 | 输入框 disabled 时（只读空间）无任何原因提示，只有按钮有 title | 给 input 也加 `:title` 说明 | safe |
| KbEval.vue:323 | 「创建中」无进行感后缀，与「新建评估」同宽跳动 | 文案改「创建中…」 | safe |
| KbEval.vue:326,450 | 创建失败是错误语义，却用 warning 配色（warning-bg/text）展示 | 改用 error 配色变量 | safe |
| KbEval.vue:333 | 「接口上线后会自动展示」——不可用时并无任何重试机制，不会「自动」展示 | 改为「请稍后刷新重试」或加自动重试 | safe |
| KbEval.vue:336,404 | 表头 `aria-hidden="true"` 且表格用 div/article 模拟，屏幕阅读器完全丢失列语义 | 用 `role="table/row/columnheader"` 或真 `<table>` | safe |
| KbEval.vue:341 | `failed` 样式靠 `/失败/.test(statusText(...))` 间接匹配翻译后的中文文案，statusText 措辞一改即失效；同写法在 377 行摘要卡又漏掉 failed 样式，两处不一致 | 直接对原始 status 判 `/failed\ | error\ |
| KbEval.vue:341-342 | 每行渲染调用 `statusText/isRunning/isCompleted` 各一次 + 内联正则一次，且随 onlyWrong 等任意响应式变化全量重算 | 在 `normalizeRun` 时预算 `statusText/状态类别` 字段 | safe |
| KbEval.vue:354 | `评估报告 · {{ reportId }}` 长 UUID 直接进 h3，无省略样式，会撑破头部布局 | h3 加 `max-width + ellipsis` 或只显示前 8 位 + title | safe |
| KbEval.vue:377 | 摘要卡状态徽章缺 `failed` 样式（列表行有），失败摘要在报告页显示为中性灰 | 补 `failed: /failed\ | error\ |
| KbEval.vue:389-392 vs 413 | 摘要卡叫「召回率（1)」，明细行叫「R@1」，同一指标两种命名 | 统一为「R@1」或「召回率（1)」 | safe |
| KbEval.vue:393 vs 389-392 | 准确率用百分比（85.0%），召回率用小数（0.750），同卡内两种量纲并列易误读 | 召回率也用 `percentText` 或统一小数 | safe |
| KbEval.vue:400-401 | 「没有错误题目→本次评估全部通过」在存在未评判（correct=null）条目不实 | 有未评判时改文案「没有判为错误的题目」 | safe |
| KbEval.vue:407 | `:key="index"`，切换「仅查看错误」筛选时 key 漂移导致行复用错乱（title/类名短暂错配） | 用 `item.question + index` 或在 normalize 时生成稳定 id | safe |
| KbEval.vue:412-414 | 内层 `v-for` 与外层同用 `index` 作用域易混淆（此处是 k），且 `[1,3,5,10][k]` 依赖位置对齐 | 用 `RECALL_KS[k]` 常量替换字面量 | safe |
| KbEval.vue:417 | verdict badge 的 `unknown` 类在 CSS 中无对应规则（靠默认灰兜底），属死类名 | 补 `.eval-verdict-badge.unknown` 规则或移除该类 | safe |
| KbEval.vue:420 | reason 为空时 `title=""` 空提示框仍挂 DOM | `:title="item.reason \ | \ |
| KbEval.vue:476,500,506,508 | 多处 `!important` 仅为压过 `.eval-row > span` 颜色，可用提高特异性替代 | 改用 `.eval-row > span.eval-id` 等写法 | safe |
| KbEval.vue:523-531 | 窄屏下表头 `display:none`，数据单元格失去列标签，移动用户无法分辨 64px 状态列与评分类 | 窄屏给单元格加 `data-label` + `::before` 伪类标签 | safe |

## tools/regression.py（37 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| tools/regression.py:16 | 只 reconfigure 了 stdout，stderr 未处理；`sys.exit(中文)`（行 27/561/563）在 cp936 管道下仍会 UnicodeEncodeError——正是头注要防的崩溃 | 加一行 `sys.stderr.reconfigure(encoding="utf-8", errors="replace")` | safe |
| tools/regression.py:8 | 多模块单行 import（`import difflib, json, os, re, subprocess, sys, socket`），与本仓其他 py 工具一行一个的惯例不一致 | 拆成一行一个 import | safe |
| tools/regression.py:9 | `from cli import cli` 依赖「脚本目录进 sys.path」，以模块方式被 import 时会撞上 PyPI 的 `cli` 包；且无注释说明 | 注释说明依赖脚本直跑，或 `sys.path.insert(0, str(Path(__file__).parent))` 兜底 | safe |
| tools/regression.py:33 | `--cases` 相对路径按 CWD 解析而非 ROOT，从 `tools/` 目录里跑会找不到题集 | 相对路径时拼到 ROOT 再 resolve | safe |
| tools/regression.py:34 | 题集 JSON 缺失/语法错误时直接抛 traceback，无友好提示（「题集坏了」和「判官坏了」分不清） | try/except 后 `sys.exit(f"题集读取失败： ...")` | safe |
| tools/regression.py:34 vs 387 | selfcheck docstring 自称「不读题集」，但模块级行 34 在 `--selfcheck` 分支（行 493）之前就读了题集——题集坏了连自检都跑不了；注释与代码不符 | 把 CASES 加载挪到 selfcheck 分支之后，或改 docstring | safe |
| tools/regression.py:55 | `RULE_KEYS` 收了 `note` 但 rules 消费循环（行 581-592）从不打印它——正是本文件自己立的「登记而不消费 = 假绿」口子 | rule 判红/判过时把 `rule.get("note")` 拼进 detail | test |
| tools/regression.py:64-101 | preflight 不校验必需 meta 键：`name`/`login`/`q` 缺失时 run_case 行 333/158、行 565 直接 KeyError traceback | key_errors 里补「缺 name/login/q」检查 | test |
| tools/regression.py:128 | 金文件 `read_text(encoding="utf-8")` 不认 BOM；带 BOM 的金文件会恒假红且 diff 看不出来 | 改 `encoding="utf-8-sig"` 读（写仍 utf-8） | safe |
| tools/regression.py:134 | `norm = lambda s: ...`（E731），且夹在两函数之间位置突兀 | 改成 `def norm(s)` 并挪到函数区 | safe |
| tools/regression.py:144 | `graph_up()` 里 `subprocess.run(["docker","ps",...])` 无 timeout 也无 OSError 兜底：docker 未安装/守护进程卡死时判官直接崩或挂住，而非 `graph=DOWN` | try/except OSError → False；加 `timeout=5` | safe |
| tools/regression.py:165 | `int(os.environ.get("DMS_REGRESSION_TIMEOUT","60"))` 遇到非数字环境变量直接 ValueError 崩 | try/except 回落 60 并提示 | safe |
| tools/regression.py:182 | JSON 解析失败时 `last = {"error": r.stdout[-300:]}` 把 stderr 整个丢掉，而 stderr 往往才有真错误 | 拼上 `r.stderr.strip()[-200:]` | safe |
| tools/regression.py:187/315/353 | 错误截断长度三处三个数（300/110/120），无依据差异 | 提一个 `TAIL = 300` 常量或注释说明各自理由 | safe |
| tools/regression.py:190 | DML 名单缺 `replace`（MySQL REPLACE INTO 是写操作）与 `into outfile/dumpfile`（SELECT 也能写文件）；首 token 检查能兜 replace 但兜不住 `select ... into outfile` | DML 补 `replace`；redline_verdict 增 `outfile/dumpfile` token 检查，并在 selfcheck 加正反断言 | test |
| tools/regression.py:248-249 | `j.get("row_count", len(...))` 取值用 fallback，但报错文案 `行数{j.get('row_count')}<...` 直接打 `None`（row_count 缺席时「行数None<5」） | 先把实际行数存变量，取值与文案都用它 | safe |
| tools/regression.py:254/258/265 | `blocks[0].get(...)` 假设 blocks[0] 必是 dict；畸形 JSON（block 为 str）时 AttributeError 崩而非判红 | `blocks[0] if isinstance(blocks[0], dict) else {}` | safe |
| tools/regression.py:268 | `raw = json.dumps(j, ...)` 无条件执行，没写 `json_contains` 的题也白序列化整份结果 | 挪进 `if c.get("json_contains"):` 内 | safe |
| tools/regression.py:272-283 | entity_fields/kpi_labels/columns/drills 四个集合同样无条件构建，55 题里多数题一个都不用 | 各包一层 `if c.get(...)` 惰性构建 | safe |
| tools/regression.py:285 vs 288/294 | 四个合同断言匹配语义不一致：`entity_fields`/`columns_contains` 精确匹配，`kpi_labels`/`drill_contains` 子串匹配，题集作者无从预期 | 注释写明差异理由，或统一为子串并在 selfcheck 补断言 | safe |
| tools/regression.py:311 | `gate_verdict` 的 subprocess 无 timeout——`ask()` 有 60s 速度门禁，gate 题卡死时整轮挂住 | 加 `timeout=60` 并对 TimeoutExpired 返回 `(False, "闸门调用超时")` | test |
| tools/regression.py:344/524 | `run_case` 对 redline 题给重试 1 次，bless 路径（行 524）只看 `llm` 键——同一条 `llm` 题两处重试口径写法不一，易改一处漏一处 | 提一个 `def _retries(c)` 小函数两处共用 | safe |
| tools/regression.py:356 | detail 里 `{j.get('elapsed_ms')}ms`，键缺席时打出 `Nonems` | `j.get('elapsed_ms', '?')` | safe |
| tools/regression.py:359 | `if j.get("rows") and j["rows"] and j["rows"][0]`——`j.get("rows")` 与 `j["rows"]` 重复判同一条件 | 删一份：`if j.get("rows") and j["rows"][0]:` | safe |
| tools/regression.py:124/538 | 题名直接拼进文件路径：`name` 含 `/` 或 `..` 时 bless 会写出 GOLDEN 目录外（题集是本地可信源，但零成本可堵） | `re.sub(r'[\\/:*?"<> | ]', "_", name)` 或校验后拒绝 |
| tools/regression.py:505 | preflight 失败时退出码 2 的语义（门没开）只在行 613 注释里解释，报错现场不说明 | 在 `sys.exit(2)` 前补一句「退出码 2 = 门没开，非题红」 | safe |
| tools/regression.py:510 | `picked = opt("--bless")`；`--bless ""` 得到空串 → falsy → 静默退化成 `--bless-all` 语义——正是 `opt` docstring 发誓要防的那类静默写操作 | `if picked is not None and not picked: sys.exit("--bless 题名不能为空")` | safe |
| tools/regression.py:507-510 | `--bless` 与 `--bless-all` 同时给时 picked 静默胜出 | 两者同现时直接报错退出 | safe |
| tools/regression.py:526 | `没拿到 SQL（{str(j.get('error'))[:100]}）`：无 error 键时打出「（None）」 | 无 error 时省略括号段 | safe |
| tools/regression.py:543-548 | `_embed_port` 解析失败静默回落 8077——而行 542 注释刚说完「写死端口会把题静默跳过」，回落本身制造了同样的静默 | 回落时 print 一行「settings.json 无 service_url，embed 端口按 8077 探测」 | safe |
| tools/regression.py:550-551 | EMBED_UP/GRAPH_UP 无条件探测：无 requires_graph 题或 `--filter` 只命中普通题时，`docker ps` 仍白跑 ~0.3-1s | 先扫 selected 是否需要 graph/embed 再惰性探测 | safe |
| tools/regression.py:554-567 | 未知 `--xxx` 参数整体静默忽略：`--fliter` 打错 = 不过滤跑全量，且行 565 的「filter 打错」预检拦不住拼错的旗标本 | 解析后校验 argv 剩余项，未知 `--` 旗标直接报错退出 | test |
| tools/regression.py:583 | `a, b = rule["lt"]`：lt 不是二元组时 ValueError traceback，preflight 未校验 lt 形状；且 lt 引用的题名不存在时只落「取值缺失跳过」恒绿 | key_errors 里校验 lt 为长度为 2 的列表且两个题名都在题集中 | test |
| tools/regression.py:595-617 | 退出码语义（0/1/2）只在注释里，使用头注（行 2-6）未写 | 头注补一行退出码约定 | safe |
| tools/regression.py:615 | 反空转闸文案列了「--filter 打错」这一成因，但该路径已被行 565-567 提前拦截，到不了这里——文案与代码不符 | 从括号里删掉「--filter 打错」 | safe |
| tools/regression.py:7 | 头注用法未提 `DMS_REGRESSION_TIMEOUT`（只在行 164 内联注释），公网跑的人发现不了 | 头注补一行环境变量说明 | safe |
| tools/regression.py:609 | 汇总行无总耗时，无法粗判这轮跑得快慢（公网 ~100s/题时尤其有用） | 起止各记 `time.monotonic()`，汇总行加 `耗时=Xs` | safe |

## admin_api.rs（35 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| admin_api.rs:66-71 | `db_err` 丢弃底层错误且**不打任何日志**——全文件 20+ 处 `.map_err(db_err)` 的 DB 故障服务端零痕迹，红线（不回浏览器）满足但运维全盲 | db_err 内 `tracing::warn!(error = %e, ...)`（只进服务端日志，不回响应） | safe |
| admin_api.rs:74-76 | `affected(n, what: String)` 迫使所有调用点在成功路径也先 `format!` 分配 | 签名改 `what: impl FnOnce() -> String`，失败才构造 | safe |
| admin_api.rs:98-101 | `strip_prefix("Bearer ")` 大小写敏感；RFC 6750 scheme 大小写不敏感，`bearer xxx` 被 401 | 按 ASCII 大小写不敏感匹配 scheme | test |
| admin_api.rs:25 | 注释「T10 把 server 拆成 api/ 目录时本文件整体平移成 api/admin.rs」——`crates/server/src/` 下无 api/ 目录，计划未落地或已作废 | 核实后删除或更新该句 | safe |
| admin_api.rs:44-46 | ROUTES 注释自称「本模块提供的路由清单」，实际缺 terms.csv/exemplars.csv/bulk status/table-enabled/schema-comments/sql-edit/llm-config/db-config 等本文件一半以上端点 | 收窄注释口径（「exemplar 纪律相关清单」）或补全清单 | safe |
| admin_api.rs:186,322,511,584,216 | `.bind(q.ds_id.clone())` / `.bind(q.status.clone())` 是不必要克隆（之后不再用 q） | 直接 move `q.ds_id` / `q.status` | safe |
| admin_api.rs:235,640 | `ds_id` 直接 `unwrap_or_else` 未 trim，`"dms "` 这类输入被白名单拒且报错文案令人困惑 | trim 后再判白名单 | test |
| admin_api.rs:247-249 | aliases 校验用 `a.trim().is_empty()` 但**存的是未 trim 原文**——`" GMV"` 入库后召回匹配可能落空 | 入库前逐条 trim（validate_term 返回清洗后值） | test |
| admin_api.rs:251 | status 未 trim，`"active "` 直接 400；与 term(L239)/definition(L243) 先 trim 的口径不一 | trim 后匹配 | test |
| admin_api.rs:405-407 | 指纹单独一次 PG 往返（`SELECT encode(sha256(...))`），与紧随的 UPDATE 可合并 | 把 `encode(sha256($n::bytea),'hex')` 内联进 EX_VALIDATE_OK_SQL，省一次往返 | test |
| admin_api.rs:510-512,583-585 | 导出达 CSV_MAX_ROWS(5000) 上限时静默截断，响应与日志都无迹象 | `rows.len() == CSV_MAX_ROWS` 时 tracing::warn | safe |
| admin_api.rs:534-536,1172-1175 | 表头比较每次 import 分配 5 个 String；`rows.remove(0)` O(n) 平移整表 | 用 `r.iter().map(String::as_str)` 比 `&[&str]`；`rows.drain(..1)` 或迭代跳过首行 | safe |
| admin_api.rs:540-569,1179-1204 | CSV 导入无行数上限（bulk status 有 500 闸），2MB body 可触发数千次串行 INSERT | 加显式行数上限（如 1000）超限 400 | test |
| admin_api.rs:556-564,1198-1202 | 导入逐行 upsert 失败只回客户端、服务端无日志（与 db_err 同盲区） | 失败行加 tracing::warn（固定分类，不带行内容） | safe |
| admin_api.rs:899-906 | `current_db_target` 声明 async 但体内零 await（`target_name()` 是同步调用） | 去 async，调用点去 `.await` | safe |
| admin_api.rs:662,699 | kv 读取 `.ok().flatten()` 把 DB 错误静默吞掉——启动/运行时开关读取失败无任何记录 | `map_err` 后 tracing::warn 再回落 | safe |
| admin_api.rs:705 | kv 目标名匹配 `n == name` 大小写敏感，与本模块 matching_key/eq_ignore_ascii_case 纪律不一致——settings 里改名大小写即静默落空走 fallback | 改 `eq_ignore_ascii_case` | test |
| admin_api.rs:713 | 硬编码回落名 `"doris_warehouse"` 魔法字符串 | 提常量并与文档互引 | safe |
| admin_api.rs:746 | `serde_json::from_str(s.extra).unwrap_or_default()` 对内建预设 extra 解析失败静默给空——预设是编译期常量，坏掉应响亮 | 失败时 tracing::warn（或 debug_assert） | safe |
| admin_api.rs:770 | llm_config 对空 model_precise 回填 model_fast，settings_api catalog(L289) 不回填——同一供应商两个端点表示不一致 | 统一在一处归一化（见 settings_api.rs:906 条） | test |
| admin_api.rs:875,881 | `db_target_capability(&cfg, name)` 对每个目标调用两次（type 与 purpose 各一次） | 算一次存局部变量复用 | safe |
| admin_api.rs:880 | `"current": *name == current` 大小写敏感，settings_api.rs:538 同语义判断用 eq_ignore_ascii_case | 统一大小写不敏感 | test |
| admin_api.rs:921 | persist_db_target 找旧 url 用 `target == &old_name` 大小写敏感，与 L918 的大小写不敏感 capability 查找自相矛盾 | 统一 eq_ignore_ascii_case | test |
| admin_api.rs:927-955 | `st.graph_status.lock().expect(...)` 重复 5 次、`chrono::Local::now().format("%F %T")` 重复 4 处 | 提 `fn set_graph_status(st, state: &str)` 一处锁一处格式化 | safe |
| admin_api.rs:997 | `enrich_dms_snapshot(...).await.unwrap_or(0)` 吞错无日志（紧邻的 seed/sync 失败都有 warn） | Err 时 tracing::warn 固定分类 | safe |
| admin_api.rs:1057,1063 | set_db_target 两次 `st.cfg()` 克隆 | 一次快照复用 | safe |
| admin_api.rs:1058 | 目标查找 `n == name` 大小写敏感；settings_api PUT 路径（matching_key, L425）大小写不敏感——PUT 用错大小写能存，POST 切换却报「未知目标」 | 统一 matching_key 式查找 | test |
| admin_api.rs:1090-1091 | question/sql 无长度上限（术语有 64/2000 闸），巨型 SQL 直接进闸门+执行+语料沉淀 | 加合理上限（如 sql ≤ 32KB）超限 400 | test |
| admin_api.rs:1148-1151 | 两条互不依赖的导出查询串行 await | `tokio::join!` 并发 | safe |
| admin_api.rs:607-608,1153 | DOC_TABLE_ROWS_SQL 选出 `domain` 列但导出端 `_dom` 丢弃不用 | SELECT 列表删掉 domain（或导出带上） | safe |
| admin_api.rs:1196-1202 | column 行失败记录只带 `table` 不带 `column_name`，定位失败行要靠猜 | failed JSON 增加 `column` 字段 | test |
| admin_api.rs:1230-1234 | 批量 disable 对全不存在的 ids 返回 `ok:true, updated:0`——与单条删除 affected() 404（F8「删除假成功」纪律）口径不一 | updated==0 时 404 或响应显式标注 | test |
| admin_api.rs:1241 | enable 循环不去重：同一 id 传两次就真实执行两遍（L394-397 每次全量取数） | `ids.sort_unstable(); ids.dedup();` | test |
| admin_api.rs:1268,1271 | `grantee_kind`/`perm` 不 trim 不转小写，`"Login"`/`"READ"` 被拒，与页面交互易踩 | trim + to_ascii_lowercase 后 parse | test |
| admin_api.rs:1277-1281,1307 | revoke 也过 ensure_ds：源被删后遗留的 kb.acl 行（L1275 注释自承无外键）永远撤不掉 | revoke 路径跳过 ensure_ds 或做孤儿清理 | test |

## crates/agent/src/run.rs（35 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/agent/src/run.rs:1 | 文件头「生成 → 五个校正器」已过时：`Correctors` 现为 8 个方法（schema_check + 7 件校正），与 run.rs:1041「七件确定性校正」自相矛盾 | 文件头与 run.rs:60 注释统一改为「schema 校正 + 七件确定性校正」 | safe |
| crates/agent/src/run.rs:6 | 文件头「EXPLAIN 只在首轮」与 steer 行为不符：run.rs:532 重组后 `attempt = 0`，EXPLAIN 会对新 SQL 再跑一次（行为本身合理，注释没跟上） | 注释补一句「steer 重组后预算归零，EXPLAIN 对新 SQL 重跑」 | safe |
| crates/agent/src/run.rs:20 | 「两次 precise 调用的用量」表述陈旧：SC 开启时 precise 调用最多 N+repair 次，都由 `chat_precise_at` 累加，不止两次 | 改为「每次 precise 调用的用量」 | safe |
| crates/agent/src/run.rs:115/165 | 锁中毒策略不一致：`run_begin`/`push_steer` 用 `.expect("steer 锁中毒")` 直接 panic，而 `run_end`(132)/`take_steers`(181)/`is_running`(147) 中毒时静默吞掉——同一把锁两种处置，panic 路径会把「信箱故障」升级成「问答 500」 | 统一成一种策略（建议全部降级为吞掉 + `tracing::error!` 一次，steer 是纯附加功能不该炸主路） | test |
| crates/agent/src/run.rs:116 | 魔法数 512（信箱表清扫阈值）无名无注，与同文件 `MAX_STEERS_PER_CONV`/`MAX_STEER_CHARS` 的命名纪律不一致 | 提为 `const MAX_STEER_CONVS: usize = 512;` 并注明「超过才兜底清扫」 | safe |
| crates/agent/src/run.rs:118 | `retain( | _, c | c.depth > 0 \ |
| crates/agent/src/run.rs:121 | `map.entry(conv_id.to_string())` 每次 `run_begin` 都分配 key 字符串，命中已有条目时白分配一次 | 先 `if let Some(e) = map.get_mut(conv_id)` 走命中分支，miss 才 `to_string()` 插入 | safe |
| crates/agent/src/run.rs:298-301 | 意图门命中（反问成立）提前 return 时，`gathered` 这个 `Result` 整体被丢弃：若 gather 实际是 `Err`，该错误不经任何日志静默蒸发（注释只说了「材料整份丢弃」，没说 Err 也丢） | return 前加 `if let Err(e) = &gathered { tracing::warn!(..) }` 或 `drop(gathered.inspect_err(..))` | safe |
| crates/agent/src/run.rs:313/456-476 | `result_print` 对每个单元格 `s.push_str(&format!("{f:.6}"))`：每个数值格一次堆分配，行数上限 MAX_ROWS、SC 多份采样时放大 N 倍 | 改用 `write!(&mut s, "{f:.6}")`（`use std::fmt::Write`），零行为变化 | safe |
| crates/agent/src/run.rs:461-463 | 指纹对 u64/i64 大整数失真：`Number::as_f64()` 对 >2^53 的 u64 返回 `Some`（精度截断），两个不同大整数（如大金额分、大 ID）会得到同一 `{f:.6}` 指纹 → SC 把不同结果误判为多数派 | `as_f64` 前先试 `n.as_u64()/as_i64()` 原样写入指纹，只在真是浮点时走 `:.6` 归一 | test |
| crates/agent/src/run.rs:339-345 | 无多数派文案两处占位符都填 `prints.len()`：「采样 {} 次得到 {} 个互不相同的结果」——但 `prints.len()` 是成功票数而非互不相同数（2v2、A/B/A/B/C 等重复票存在时措辞失实），且与 run.rs:276 文档「N 个各不相同」同源不准 | 用 `prints.iter().collect::<HashSet<_>>().len()` 填第二处，采样次数填 `d.sc_samples` | test |
| crates/agent/src/run.rs:305 + 1369-1373 | `threshold_is_strict_majority` 测的是测试内字面公式 `n / 2 + 1`，不是生产表达式：run.rs:305 若被改成 `d.sc_samples / 2`，该测试依然全绿（自证型哑测试，与本文件 396-406 行自己批判的「恒真坑」同族） | 把门槛提成 `fn majority_need(n: usize) -> usize`，生产与测试同调它 | test |
| crates/agent/src/run.rs:504/543/257 | `Round.t0` 字段是 `cx.t0` 的纯复制（504 行 `let t0 = cx.t0`），而 `Round` 已持有 `cx`——冗余字段，两处同步靠人肉 | 删 `Round.t0`，execute 里用 `self.cx.t0` | safe |
| crates/agent/src/run.rs:505-508 vs 597-608 | `run_once` 与 `steer_regen` 重复同一串「State 初始化 → schema_fix → correct_chain」样板，未来加一步要改两处 | 抽 `async fn fresh_state(cx, d, out: GenOut) -> State` 收口 | safe |
| crates/agent/src/run.rs:620 vs 1026 | `ensure_limit(&st.sql, dialect)` 在 `schema_fix`(1026) 算一次只用于喂 schema_check，`attempt`(620) 又对同一 SQL 重算一遍；caliber Retry 形状被拒后 sql 未变，下一轮 620 还再算 | schema_fix 后把结果存回 `st.candidate` 或仅在 sql 变化时重算 | safe |
| crates/agent/src/run.rs:644/659/795 | `repair_round(...).await?`：repair（LLM）本身失败时原始错误（闸门拒绝详情/EXPLAIN 错误/MySQL 错误）被整个丢弃，上抛的是 LLM 错误——排障时看到的报错与根因无关，误导性强 | `?` 前 `.map_err( | re |
| crates/agent/src/run.rs:656 | EXPLAIN 的 `Err`（连不上池）与 `Ok(None)`（超时/抖动）两支完全静默，连 debug 都没有——「预检层今天到底跑没跑」从日志不可证伪，与 run.rs:559-563 自己批判的「静默与无事发生同形」同病 | 加 `tracing::debug!`（不升级 warn 以免噪音），写明「EXPLAIN 预检跳过：原因」 | safe |
| crates/agent/src/run.rs:680 | `if let Some(n) = note` 的 `n` 遮蔽了入参尝试轮次 `n: usize`（671），同函数内两个 `n` 两种含义 | 改名 `if let Some(text) = note` | safe |
| crates/agent/src/run.rs:690/705 | `output_shape(&st.candidate)` 与随后的 `keeps_output_shape(&st.candidate, &rewritten)` 对同一 candidate 各做一次 AST 解析（keeps 内部再 parse 一次 before），一次口径回炉重复解析两遍 | `keeps_output_shape` 加重载收预算好的 `before` 形状，或接受重复但注释说明（微） | safe |
| crates/agent/src/run.rs:713 | warn 里手写 `rewritten.chars().take(400).collect::<String>()`，与本文件 1216 行的 `clip()` 工具同义重复 | 换 `clip(&rewritten, 400)` | safe |
| crates/agent/src/run.rs:744-746 vs 794-797 | 取证不对称：闸门拒绝（642)、EXPLAIN 失败（657）首轮都落 `correction_log`，唯独首轮执行错误（794）不落任何痕迹——「模型首版 SQL 执行报什么错」无取证材料，与 636-640 行注释批判的正是同一类盲区 | 首轮执行失败也 `log(cx, "exec-error", &e.to_string())`（新 kind，需同步 correction_kinds 判据） | test |
| crates/agent/src/run.rs:778 | 经验蒸馏 `let _ = save_memory(...)` 把 `anyhow::Result` 整个吞掉，写 PG 失败零痕迹（exemplar.rs:214 的「刻意吞错」纪律是针对纯观测写入，但那里至少语义明确，这里连 warn 都没有） | 改 `if let Err(e) = ... { tracing::warn!("经验蒸馏落库失败： {e}") }`，不传播 | safe |
| crates/agent/src/run.rs:784 | `st.route.clone()` 后 `st.route` 再无人读（785 取 note、787 取 alt 后 st 即弃） | `std::mem::take(&mut st.route)` 省一次分配 | safe |
| crates/agent/src/run.rs:788 | `st.alt_questions.clone()` 同上，整 Vec<String> 克隆后原值即弃 | `std::mem::take(&mut st.alt_questions)` | safe |
| crates/agent/src/run.rs:827-828 | 注释「已有同问句语料 → 不重复复核」掩盖了第二种 false：`save_with_context`（exemplar.rs:196）对 PG 错误 `unwrap_or(false)`——DB 抖动时这里把「写库失败」当「语料已存在」处理，静默跳过复核 + 向量回写，零日志 | 注释补「含 DB 失败」；让 `save_with_context` 返 `Result<bool>` 或此处至少 debug 留痕 | test |
| crates/agent/src/run.rs:954 | `sort_by_key(Reverse(w.chars().count()))`：`sort_by_key` 每次比较都重算 key，`chars().count()` 又是 O（词长）——词表大时是 O(n log n · L) | 先算好 `(Reverse(len), item)` 再 `sort_by_key( | (k, _) |
| crates/agent/src/run.rs:1024-1038 | `schema_fix` 里 `repair` 的 `Err` 被 `if let Ok(fixed)` 静默吞掉（1034）——同函数 1030 行 schema_check 失败都有 warn，repair 失败反而无痕；run.rs:382 的判据只断言「含 warn」，盖不住这个分支 | `else` 分支加 `tracing::warn!("schema-fix 自修失败（保持上一版 SQL 继续）: {e}")` | safe |
| crates/agent/src/run.rs:1049/1054/1059/1096 | 四件投影/WHERE 级校正的日志只记**改前** SQL（`补分组列进投影：{旧sql}`——从详情里看不出补了什么），而 1067/1077/1087 三件都记「旧 → 新」；同为 correction_log 详情两种形态 | 四处补齐 `→ {clip(&fixed, 120)}`，与 agg-fix 等同形 | safe |
| crates/agent/src/run.rs:1105-1111 | `pub async fn generate_sql` 全仓零调用点（仅 lib.rs:59 re-export 与注释引用），是死公开 API；且它绕过 schema_fix/correct_chain/闸门直出 SQL，留着等于留一条会被误用的裸生成通道 | 删除函数及 lib.rs:59 的 re-export（或注明保留理由） | safe |
| crates/agent/src/run.rs:1143 | `snapshot = (pc.schema.clone(), side_info_of(pc))` 在 `generate_sql_for` 里每次生成重算：SC N 次采样共享同一份 `g`，却 clone/拼接 N 遍同一字符串 | snapshot 在 `run_llm` 预取一次随 `g` 传入，`GenOut` 改持有引用或 Arc | safe |
| crates/agent/src/run.rs:1144/1167 | `build_system_prompt(cx.p, &today_cn(), dialect)` 每次生成与每次 repair 都重建整份系统提示词（同一次问答内三个入参恒定）；`today_cn()` 同句多问重复取时钟 | `run_once` 入口算一次 `system`/`today` 往下传（repair 同理） | safe |
| crates/agent/src/run.rs:1151 | 日志字段名 `prompt_chars` 实为 `system.len() + user.len()` 即**字节数**——中文 prompt 下字节≈3×字符，按字段名读数会误判 prompt 规模 | 改名 `prompt_bytes` 或换 `chars().count()` | safe |
| crates/agent/src/run.rs:1157 | `tables.clone()` + `alt_questions.clone()`：SC 第 2..N 次采样的这两份克隆只有 `run_once` 首用（tables 喂 513 行 rules、alt 进 State），N 份采样重复克隆同Vec | `GenOut` 对这两域改借用 `&'a [String]`（g 活得比它久），仅 `run_once` 首份消费 | safe |
| crates/agent/src/run.rs:1164 | `repair` 每次调用都 `gather_all_cards` 全量重召回（embed + 多路 PG IO）：一轮问答内 repair 最多被调 3+ 次（schema-fix/口径×2/闸门/EXPLAIN/执行），SC 下再乘 N——召回材料在同一轮内本无变化 | 每轮（run_once）缓存一次 cards 复用；注意 gather 有经验命中计数等遥测副作用，调用次数变化需带测试 | test |
| crates/agent/src/run.rs:1206-1209 | `log()` 文档「九个 kind 一个不少：六个字面量在本文件」严重过时：实际本文件字面 kind 已有 12 个（新增 select-fields-fix/dedup-select-fix/time-lower-bound-fix/gate-blocked/steer-applied/steer-failed），加 guard 三个共 15 个；`correction_kinds_all_present`(1476) 的 LITERALS 也只守 6 个，其余 6 个 kind 静默断供不会被判据抓到 | 文档改为如实清单；LITERALS 补齐 12 个字面量（断言逻辑不变） | test |

## caliber.rs（34 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| caliber.rs:全文件 | 与 model.rs 同款 CRLF/LF 混杂行尾 | 统一行尾 | safe |
| caliber.rs:122-161 | `build_rules` 9 条查询全部串行 `await`（问数热路径），而拼接顺序由代码固定不受并发影响 | `try_join!` 并发加载 | test |
| caliber.rs:66-82 | 11 元组手写类型标注 | 本地 struct | safe |
| caliber.rs:267-269 | `code_eq_rules` 的 `name == code` 再判与 `load_code_eq_values`:248 SQL `name <> code` 重复——是纯函数自卫但无注释 | 补一句注释 | safe |
| caliber.rs:334 | `enum_rules` `vs.contains(&pair)` O(n²)：775 值列约 30 万次字符串对比较 | `HashSet` 判重 | safe |
| caliber.rs:442-443 | `code_rules` `longest_value_hit` 命中后再 `find` 一次拿 code | 让 `longest_value_hit` 返回索引/二元组 | safe |
| caliber.rs:447 | 歧义判定 `hits.iter().filter(..).count()==1` O(n²) | 预建 code→count 映射 | safe |
| caliber.rs:504-510 | `matched_ratio`（全指标 `match_word` 扫问句）在判 `PERCENT_WORDS` 之前无条件计算 | 先判词再算，短路 | safe |
| caliber.rs:574 | `scope_filter.to_uppercase().contains("SELECT")` 每指标每问句分配大写串；且 `'SELECTED'` 类字面量误命中（静默漏规则方向） | 大小写无关扫描 + 词边界判定 | test |
| caliber.rs:589 | `!m.dedup_keys.is_empty()` 缺 `trim()`：空白串声明会产出 keys 为空的 `RequireDedup`（572/610 行同类守卫都有 trim） | 对齐 `.trim().is_empty()` | test |
| caliber.rs:660 | `base_table` 只 trim 反引号不 trim 双引号，mod.rs:159 `catalog_ident` 两者都 trim——不一致 | 对齐 | test |
| caliber.rs:168-171 | `fanout_keys` `card` 精确匹配 `N:1`/`1:N`，种子写成小写 `n:1` 静默漏键（漏判方向无告警） | 大小写无关匹配 + 未知 card 值 `warn!` | test |
| caliber.rs:295 | `is_recalled` 闭包内 `bare(t)` 对每个 `r` 重算 | 提出循环算一次 | safe |
| caliber.rs:1203 | 测试函数签名与首条语句挤一行（`{        let edges`），格式事故 | rustfmt 该函数 | safe |
| caliber.rs:39-58 | `CaliberMetric` 无 `Debug`（规则排障常要打印指标） | 补 derive | safe |
| caliber.rs:1021 vs 641 | 注释自称「复用 `time_ish_conds` 里那个 `ish`」，实际是复制粘贴第三份（1031 行内联） | 抽 `fn looks_timeish(c: &str) -> bool` 三处共用 | safe |
| caliber.rs:369-371 | 同一个 `wrong.join(" / ")` 在一条 format! 里算三遍 | 先 `let w = wrong.join(" / ");` 再引用 | safe |
| caliber.rs:672-674 | `constrained` 每个候选列都重建 `(a.clone(), c.clone())` 元组做 contains；`table.to_lowercase()` 也在循环内重复 | 改 `cond_cols.iter().any( | (p,x) |
| caliber.rs:631-634 | `aliases_of` 每次 clone 出 `Vec<String>`；只读比较不需要所有权 | 改返回 `Vec<&str>`（内部签名，行为同） | safe |
| caliber.rs:655-657,672 | `base_table_count()` 在 `constrained` 里按列重算 HashSet | 在 judge 的列循环外算一次传入 | safe |
| caliber.rs:299-302 | `.then( |  | viol(...)).flatten()` 绕一层 Option |
| caliber.rs:451 | hint 里反引号不配对的：`` `{want} = '{code}'，其余一字不动 `` 缺收尾反引号，markdown 渲染会泄格式 | 补上收尾 `` ` `` | safe |
| caliber.rs:707-718 | DISTINCT 列提取对任意表达式 `to_string()` 后 `rsplit('.')`：`COALESCE(d.a,'')` 会产出 `"a, '')"` 这类垃圾键进 `distinct_cols`（永不命中 keys，属脏数据） | 只收 Identifier/CompoundIdentifier，其余跳过 | test |
| caliber.rs:713 | `rsplit('.').next().unwrap_or(&expr)`：rsplit 恒非空，死分支 | 删 unwrap_or | safe |
| caliber.rs:736-738 | HAVING/QUALIFY 以 `cond=false` 采集：`HAVING col = '码'` 不算约束——漏判方向但未像其它漏判一样注释声明 | 注释声明，或按条件采集（行为变化） | test |
| caliber.rs:290 | `ranked && eq_one` 是全局旗标组合：一个分支的 ROW_NUMBER + 另一分支任意 `x = 1` 即放行 RequireLatest，超出 10 行注释描述的形态 | 旗标按 Query 作用域配对（如记录 ranked 所在层级） | test |
| caliber.rs:995 | `times_100` 与 `divide` 不绑定：投影里任意 `*100`（哪怕与除法无关）就让 RequirePercentScale 放行 | 记录乘 100 是否出现在除法子树内 | test |
| caliber.rs:996-1001 | `eq_one` 只认 `col = 1`，不认 `1 = col`（binop 只查 right） | 对称补 left 字面量臂 | test |
| caliber.rs:138,143,804,1077 | `trim_matches('`')` 对 sqlparser 的 `Ident.value` 恒为 no-op（parser 已去引号），四处死防御 | 删或集中注释说明「防御未来手写 AST」 | safe |
| caliber.rs:812-813 | `t` 被 clone 两次（key 回退一次、push 一次） | 重排：`let t=...; let key=alias...unwrap_or_else( |  |
| caliber.rs:1054-1061 | `first_arg_column` 的 `}?` 遇首个非 Expr 参数（如 Wildcard）整体 bail，其后参数里即使有列也放弃 | `?` 改 `continue` 语义（match 落空 continue） | test |
| caliber.rs:161-169 | parse 失败静默返空与 1204-1208 测试对齐，但运行期零观测：口径校验「整场弃权」无 warn 痕迹 | 在 parse 失败处加 `tracing::debug!`（kernel 目前无 tracing 依赖则先注释 TODO） | safe |
| caliber.rs:204-218 | `drop_conflicting_time_cols` 冲突判定用全列 sort+dedup，规则数小但每轮都重建 | 微：用 HashSet 判重即可，可不动 | safe |
| caliber.rs:1189-1191 | `rules_of` 测试 helper 每次重建 Vec——测试内无碍；但 1193-1202 `facts()` 与 `check_caliber` 的扫描是两套入口，方言漂移无防线 | 注释钉一句「两处方言必须同为 GenericDialect」 | safe |

## graph.rs（33 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| graph.rs:101,106,115,120,147,218,341 | RwLock 中毒直接 `.expect("graph state 锁中毒")` panic；同 crate 的 registry.rs:162 已用 `into_inner()` 恢复，口径不一 | 统一改 `unwrap_or_else(\ | e\ |
| graph.rs:145-151 | `adopt_if_current` 把真实 DB 错误（断网/权限）与「标记不匹配」一起吞成 `false`，排障无迹 | `_ =>` 分支加 `tracing::debug!` 带 error | safe |
| graph.rs:163 | `esc` 把 `\` 整体删除会静默篡改含反斜杠的名字，且不处理换行等控制字符（可撑破 Cypher 单引号字面量） | 文档化或过滤控制字符 | test |
| graph.rs:163 | 两次 `replace` = 两次全串分配 | 单趟 `chars().fold`/显式循环一次成型 | safe |
| graph.rs:239 vs 17 | SQL 字面量 `LIMIT 250001` 与 `GRAPH_EDGE_LIMIT = 250_000` 手写 +1 关系，改常量即漂移（`raw_all` 要 `&'static str` 不能 format!） | 加 `const _: () = assert!(250001 == GRAPH_EDGE_LIMIT + 1);` | safe |
| graph.rs:244-247 | 超限报错只有上限没有实际行数，现场要靠猜 | 文案带 `edges.len()` | safe |
| graph.rs:254-261 | province 查询无 LIMIT/硬上限，与主边查询（239/244）防护不对称，主档膨胀可拖垮内存 | 加 LIMIT + 同款 `ensure!` | test |
| graph.rs:257 | SQL 串里大段连空格（`region_name              FROM`），排版事故 | 折叠空白 | safe |
| graph.rs:262-269 | `customer_sales_region` 对 ≤250k 行先 clone `(code, region)` 再进 HashSet 去重 | 先用 `HashSet<(&str,&str)>` 借用去重，幸存者再 clone | safe |
| graph.rs:283-284 | `entry(cc.clone()).or_insert_with(\ | \ | cn.clone())` 命中也白 clone key，25 万行每次都付 |
| graph.rs:289 | `let _ = drop_graph(...)` 裸吞错且无注释：首跑「图不存在」是预期，其余错误不是 | 注释 + `tracing::debug!` 记录错误 | safe |
| graph.rs:298 | `CREATE INDEX` 失败 `let _ =` 静默——索引缺失会让每条建边 MATCH 全表扫 | 失败时 `tracing::warn!` | safe |
| graph.rs:310-313 | 注释明写「这一段失败不该让整次同步失败」，代码却是 `batch_dim_edges(...).await?`——注释与代码相反 | 按注释改为 warn-and-continue，或删该句注释 | test |
| graph.rs:316-321 | provinces 去重计数在此重算一遍，而 `batch_dim_edges`(597-602) 内部已算过 sales_regions 同款集合 | 让 `batch_dim_edges` 把两个计数都返回 | safe |
| graph.rs:340-351 | 先 `mark_ready`(340) 后写 GraphMeta 持久标记（347-351)：标记写失败时进程返回 Err 但内存态已 ready、CLI 又 adopt 不了，两半不一致 | 先写标记再 `mark_ready`，或失败时回滚状态 | test |
| graph.rs:364 | `asset.code == format!("{}.{}", db, table)` 每个资产一次 String 分配 | 用 `strip_prefix`/长度比较零分配校验 | safe |
| graph.rs:373 | `collect::<Vec<_>>()` 仅为数个数（456 才用 [0]） | `.filter(...).count()` 计数 + `find` 取首个 | safe |
| graph.rs:412-419 | `layers` 已预置全部 5 层，循环里 `layers.insert(asset.layer...)` 永远插不进新值（368 已校验 ∈ 5 层）——死代码 | 删该 insert（domains 的保留） | safe |
| graph.rs:422 vs 460 | MetricContract 节点在 422 建一次、460 又 MERGE 同一节点，冗余 | 只留 460-462 带 SET name 的那份 | safe |
| graph.rs:466 | 返回 `assets.len()*2+1` 是「尝试数」非「实建数」，返回值语义未标注 | 注释说明口径 | safe |
| graph.rs:478 | `esc(value)` 同一值调两次 | 绑定一次复用 | safe |
| graph.rs:497-500 | 单据族 code 重复时 HashMap 静默覆盖（last wins），而目录侧（371）对重复是 `ensure!` 拒绝——口径不一 | 加同款重复 code `ensure!` | test |
| graph.rs:503-506 | `entry(x.clone()).or_insert_with(\ | \ | x.clone())` key/value 同串双 clone |
| graph.rs:540,594 | `count += chunk.len()` 记的是尝试条数；维度边 MATCH 不到会静默跳过（311 注释自认），日志 `dim_edges`/`schema_edges` 系统性高估 | 日志字段改名 attempted，或用 RETURNING 实计 | safe |
| graph.rs:561-562 | `HashSet<&String>` + `Vec<&&String>` 双重间接 | 改 `HashSet<&str>` 直接迭代 | safe |
| graph.rs:563,475,582,612,630 | chunk 尺寸 1000/200/500 混用无依据 | 统一或注释取舍理由 | safe |
| graph.rs:653 | `trim_matches('"')` 会把名字里真·首尾引号全剥掉，内层 `\"` 也不反转义 | 改 `strip_prefix`/`strip_suffix` 各一次 | test |
| graph.rs:666,678,690,763 | `=~ '.*{}.*'` 只转义引号/反斜杠，用户词里的正则元字符（`.` `*` `[` `(`，如 "C++"、"A.B"）改变匹配语义 | 插值前正则转义 | test |
| graph.rs:663-698 | `limit` 无上限直接拼进 Cypher | 防御性 clamp | test |
| graph.rs:731→757-799 | 每个窗口串行 `resolve_one`，每个 label 重新 `age_conn`（769）= acquire + LOAD + SET 三连，最坏 3×/窗口 | 把一次 `age_conn` 提出循环复用 | test |
| graph.rs:773-776 | `Err(_) => continue` 把「label 不存在」与真实 DB 错误混为一谈，静默降级 | continue 前 `tracing::debug!` 带 error | safe |
| graph.rs:778-779,859-861 | 解码失败经 `.ok().flatten().unwrap_or_default()` 静默变空串/0.0 | 失败时 debug 日志 | safe |
| graph.rs:736 | `sort_by_key(start)` 看似冗余，实则 `candidate_windows`（kernel text.rs:152-155）按长度外层、起点内层产出，本就不按 start 有序——缺注释易被「清理」掉 | 加一句「为何必须排序」注释 | safe |

## KbAnswer.vue（32 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| KbAnswer.vue:40 | `conflictingFamilies` 每个成员 `slice(i+1)` 再 `some`，O(n²) 且产生 n 次数组拷贝 | 改为双下标 `members.some((m,i)=>members.some((o,j)=>j>i&&governedVersionsConflict(m,o)))`，零分配 | safe |
| KbAnswer.vue:46 | `hasVersionRisk` 对**原始** markdown 跑版本正则，未走 `cleanMarkdown`；证据段/代码块里的「版本与差异」字样会误触发告警横幅 | 改为对 `displayMarkdown.value` 测试 | test |
| KbAnswer.vue:84 | `localStorage.getItem` 在隐私模式/禁用存储时抛 SecurityError，直接击穿 `loadFeedback` | try/catch 包住，失败按无缓存处理 | safe |
| KbAnswer.vue:104-105 | 401 已 `emit('auth-expired')`，随后 `!response.ok` 又 throw，catch 里再补一条「反馈提交失败」——会话过期场景下文案误导 | 401 时 emit 后直接 return | safe |
| KbAnswer.vue:107 | `localStorage.setItem` 配额满抛错会被 catch 成「反馈提交失败」，但服务端其实已落账，用户重试产生重复 upsert | setItem 单独 try/catch，不影响成功态 | safe |
| KbAnswer.vue:115-126 | watch 重置了 opened/loading/errors/stale 等，但没重置 `feedbackBusy`；答案切换瞬间反馈请求在飞时旧 busy 虽会由 finally 清掉，但期间新答案的反馈按钮被误禁用 | watch 里一并 `feedbackBusy.value = false`（在途请求的 finally 幂等） | safe |
| KbAnswer.vue:139 | `esc` 不转义 `"`，KbDocPreview.vue:213 的 `esc` 转义——两处同名函数口径不一 | 抽公共 util 或至少统一实现 | safe |
| KbAnswer.vue:146 | 所有引用按钮 aria-label/title 都是同一个「查看来源原文」，读屏用户无法区分第几条 | 拼上序号：`aria-label="查看来源 ${index} 原文"` | safe |
| KbAnswer.vue:149-150,176-177 | `[KPI | SEC | CON]-xxx` 剥离正则一字不差写了两遍（`inline` 与 `cleanMarkdown`），未来改口径容易漏一处 |
| KbAnswer.vue:159 vs 231 | `cleanMarkdown` 标题识别 `#{1,6}`，`render` 只认 `#{1,4}`：未被隐藏的 `#####` 标题会被渲染成字面 `# 文字` 段落 | 两边统一为 `#{1,6}`（render 里 `Math.min(6, len+2)` 已兼容） | safe |
| KbAnswer.vue:154-180 | `cleanMarkdown` 不跟踪 ``` 围栏：代码示例里若有 `# 证据` 或 `bm25: 0.87` 行会被静默删掉 | 增加围栏状态机，围栏内不剥离 | test |
| KbAnswer.vue:191 | 数字单元格单位闭集缺 `台/件/条/人/吨/公里` 等常见单位，这些列失去右对齐 | 扩充单位候选或放宽为 `[\u4e00-\u9fa5]{1,3}` | safe |
| KbAnswer.vue:231 | 标题正则 `^(#{1,4})` 不允许前导空格，CommonMark 允许 0-3 空格；`cleanMarkdown`（L158 trim 后判断）却认——同一段md两边解析不一致 | render 侧改 `^\s{0,3}(#{1,4})` | safe |
| KbAnswer.vue:234 vs KbDocPreview.vue:241 | 标题降级偏移一处 +2（h1→h3）、一处 +1（h1→h2），无注释说明是有意差异 | 统一或加注释说明各自理由 | safe |
| KbAnswer.vue:253 | `keyLine` 只认 `结论 | 答案 | 建议 |
| KbAnswer.vue:266 vs 273 | 兜底标题两个：无标题时「回答摘要」，命中标准标题时「知识库回答」——同一组件两种缺省名 | 统一为一个 | safe |
| KbAnswer.vue:269-285 | `presentation` 同样不跟踪 ``` 围栏，代码块里的 `# 注释` 可能被抢当标题/摘要 | 与 cleanMarkdown 共享围栏状态 | test |
| KbAnswer.vue:315-339 | `downloadSource` 无 busy 闸：连点「下载原件」触发多个并发下载；KbDocPreview.vue:368 有 `downloading` 闸——同款动作两文件口径不一 | 加 per-doc 或全局 downloading ref，禁用中按钮 | safe |
| KbAnswer.vue:321-322 | 401 emit 后继续 throw，catch 置「原件暂时无法下载，请稍后重试」——认证过期时文案误导 | 401 emit 后 return | safe |
| KbAnswer.vue:333 | `setTimeout(...,0)` 就 `revokeObjectURL`，Safari 等浏览器下载可能尚未开始即被回收 | 延到 1000ms+ 或监听页面 visibility 后回收 | test |
| KbAnswer.vue:352 | 高亮清除定时器不校验 `answerGeneration`：答案切换后旧定时器可清掉新答案同序号的高亮 | 闭包捕获 generation，清除前比对 | safe |
| KbAnswer.vue:382-397 | `sessionHeaders()` 抛出的「登录会话已失效，请重新登录」被通用 catch 覆盖成「原文暂时无法加载」；KbDocPreview 侧（L200）是展示 `e.message`——两文件口径不一且此处更差 | catch 里优先用 `e instanceof Error ? e.message : ''`（与 KbDocPreview 对齐） | safe |
| KbAnswer.vue:417 | `v-if="result.markdown"` 对纯空白字符串（`'  \n'`）为真：渲染出只有通用标题、无摘要无正文的"空壳"头部 | 改判 `displayMarkdown`（已 trim）非空 | safe |
| KbAnswer.vue:421 vs 454 | 同一数量一处「综合 N 份**资料**」一处「N 份**文档**」 | 统一用词 | safe |
| KbAnswer.vue:423 | 标题层级硬编码 `<h3>`，与所在页面层级无关联，可能破坏大纲（父页面若无 h1/h2） | 用 `role="heading" :aria-level` 或按上下文定级 | safe |
| KbAnswer.vue:433-434 | 👍/👎 按钮的选中态只有视觉 class，读屏不感知 | 加 `:aria-pressed="feedback === 'correct'/'data'"` | safe |
| KbAnswer.vue:435 | 「已反馈，感谢」无 live region，读屏用户收不到提交成功反馈 | 加 `role="status"` | safe |
| KbAnswer.vue:458 | `aria-label="回答来源"` 挂在无 role 的 `<div>` 上，多数读屏直接忽略 | 加 `role="list"`（article 配 `role="listitem"`）或去掉该 label | safe |
| KbAnswer.vue:464 | `aria-expanded` 没有配套 `aria-controls` 指向展开区 | 给预览区加 id 并引用 | safe |
| KbAnswer.vue:553-557 | 引用按钮 `font-size: 9.5px`，低于可读性下限（通常 ≥11px） | 提到 10.5-11px，行内高度同步 | safe |
| KbAnswer.vue:73-77 | 注释说「👍 映 'correct'（服务端自旋 resolved）」——「自旋」疑为「自动置」之类笔误，语义不通 | 核对服务端行为后改写注释 | safe |
| KbAnswer.vue:497,576,592,623 | 元信息字号 10-10.5px 多处，移动端可读性差 | 统一提到 ≥11px 或至少在媒体查询里放大 | safe |

## tools/evaluation.py（32 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| tools/evaluation.py:833 | 裸 `main()` 无 `if __name__ == "__main__"` 守卫（kb_eval:919、deep_contract:201 都有），任何 import 即跑全量评测 | 加守卫 | safe |
| tools/evaluation.py:54 | eval_cases.json 缺失/无 "cases" 键时裸 traceback；kb_eval:67-68 是干净退出 | 同款友好退出 | safe |
| tools/evaluation.py:41 | 一行导入 9 个模块且 `csv` 排在末尾乱序 | 拆行按字母序 | safe |
| tools/evaluation.py:779+ | 无 `--help` 分支；打 `--help` 会被静默忽略并直接跑 40 分钟全量 | 加帮助分支（可复用头部注释块） | safe |
| tools/evaluation.py:779+ | 未知参数静默忽略——kb_eval:53-62 已有同款硬失败闸，且该注释自述「这一族咬过两次」，本文件恰是漏网者 | 加 `_KNOWN_FLAGS` 校验 | safe |
| tools/evaluation.py:87,274 | `elapsed_ms` 在 274 行才定义，EvalBatch 初始化先用后定义（运行期无碍但阅读跳跃） | 函数定义上移到类前 | safe |
| tools/evaluation.py:139-142 | `json.loads(item)` 可能返回 list/标量，142 行 `out.get("id")` 直接崩 | isinstance 守卫，非 dict 抛 BatchError | test |
| tools/evaluation.py:201-202,180 | `_run_once` 不校验解析结果是 dict，run() 180 行 `out.get("error")` 对 list 响应崩 | _run_once 内统一 dict 化 | test |
| tools/evaluation.py:196-200 | stderr/stdout 均空时 err=""，falsy 导致「无错误」假象 | 兜底 `f"rc={r.returncode}，无错误输出"` | safe |
| tools/evaluation.py:207-213 | `ask(retries=1)` 与 run(tries=3) 重试叠加最多 6 次冷启动，且 `retries` 全仓无人传参（死参数） | 删 retries 形参或注释叠加意图 | safe |
| tools/evaluation.py:251-271 | case_protocol_errors 不校验 `login`/`q` 必填；batch_payload:301/303 运行期 KeyError | 预检补 login/q 非空校验 | test |
| tools/evaluation.py:329 | `lstrip("¥$")` 只剥行首符号，`-¥5`、`€5` 剥不掉，呈现差异变判红 | 注释约定支持范围或扩符号集 | safe |
| tools/evaluation.py:331,341-347 | `float("nan")`/`inf` 字面量被当数字比；nan≠nan 在 close() 里恒红，双 NaN 单元格永假红 | 非有限浮点按字符串比 | test |
| tools/evaluation.py:368-371 | 列数只校验首行；后续行参差不齐时 zip 静默截断，多余单元格不参与比对可假绿 | 逐行长度校验或取 max 列数比 | test |
| tools/evaluation.py:373 | 「第{i+1}行」是排序后序号而非原始行号，排查者对不上原始结果 | 文案注明「排序后」 | safe |
| tools/evaluation.py:395 | 聚合正则无 `\b`，`checksum(`/`account(` 内的 sum(/count( 被误收入 agg 集 | 加 `\b` 边界 | safe |
| tools/evaluation.py:402-409 | 两 SQL 组件全同时也返回 "select"，把行数不等类失败误导成 select 差异 | 全同时返回 "-"/"组件相同" | safe |
| tools/evaluation.py:484 vs 556 | batch 错误截断 160、legacy 截断 100，同款两处不一 | 统一常量 | safe |
| tools/evaluation.py:530 | 手工零时钟 dict 与 timing_of() 输出同形，重复维护 | 用 `timing_of()` 或模块级常量 | safe |
| tools/evaluation.py:533 | `assert row is not None` 会被 `python -O` 剥掉，闸失效 | 改显式 `raise AssertionError`/`RuntimeError` | safe |
| tools/evaluation.py:566-567 | 无 error 但不可展示时 detail 打成「生成失败： None」 | 分支文案：「生成失败： 响应缺少可展示结果」 | safe |
| tools/evaluation.py:595-597,530 | p50/p95 把失败行的零时钟也计入分位数，失败多时延迟基线被 0 拉低 | 只统计 `ok is not None` 或非零时钟行 | test |
| tools/evaluation.py:609-611 | 并发污染提醒只在 runs>1 打印，单趟同样会被污染 | 无条件打印或注释说明 | safe |
| tools/evaluation.py:621 | 无 tags 时打印裸「分层：」前缀 | 空则跳过 | safe |
| tools/evaluation.py:631-632 | `git rev-parse` 不查 returncode，git 缺席时 commit 静默为空写进基线 | 失败时写 "unknown" | safe |
| tools/evaluation.py:644-651 | 基线迁移静默丢弃列数 <7 的旧行，数据丢失无提示 | 打印迁移摘要（迁移 N 行/丢弃 M 行） | safe |
| tools/evaluation.py:734-738 | selfcheck 用源码字符串断言 `for attempt in range(3)` 等字面量，提取常量/重命名即假红且与行为无关 | 改行为级断言或放宽正则 | safe |
| tools/evaluation.py:784-786 | `int()/float()` 解析 `--runs`/`--timeout-seconds` 无兜底，打错值裸 ValueError traceback | try/except 友好退出（对齐 arg() 的风格） | safe |
| tools/evaluation.py:790 | 进度文件在筛题/协议预检（795-803）之前就清空，打错 --filter 也丢旧进度证据 | 挪到 cases 校验通过之后 | safe |
| tools/evaluation.py:807-812 | 注释「flush 不可省：不 flush 就等于没有进度文件」但 `with` 关闭本就 flush，注释与代码关系误导 | 改注释：flush 防的是未来去掉 with 的改法 | safe |
| tools/evaluation.py:819 | `runner` 三态选择在每趟循环内重复计算 | 挪出 for 循环 | safe |
| tools/evaluation.py:17-20,439-440 | 头注释与 quiet_alarm 注释硬编码「24%」「9/38」等实测数字，题集扩容后 silently 过期 | 注明测量日期/题集规模出处 | safe |

## artifact_api.rs（32 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| artifact_api.rs:47-49 | `db_err` 把驱动错误整个丢弃且**全文件零日志**：DB 故障对外 500「请稍后重试」，运维侧无任何线索 | 函数内 `tracing::warn!(err = %e, "artifact DB 操作失败")` 再返回固定文案 | safe |
| artifact_api.rs:70 | 注释称「撞号 = 第二次写入报错重试」，但 115-118 行直接返回 `产物写入失败`，代码里没有任何重试 | 改注释为「撞号 = 第二次写入报错（调用方重试）」，或捕获唯一键冲突重试一次 | safe |
| artifact_api.rs:94 | `JOIN chat.conv c ON c.id::text = a.conv_id`：对主键列做 `::text` 转换使 PK 索引不可用于 join | 加 `a.conv_id ~ '^\d+$'` 护栏后改 `c.id = a.conv_id::bigint`，带验证 | test |
| artifact_api.rs:137 | `let _p = ...` 下划线前缀惯例表示「未使用」，但 146、162 行都在用，误导读者 | 改名 `p` | safe |
| artifact_api.rs:153 | `"html" => req.content.clone()` 复制整页 HTML；`req` 是 owned 值此后只用 title（160 行，在此之前已可取） | 调整取值顺序后 `std::mem::take(&mut req.content)` | safe |
| artifact_api.rs:253-256 | `conv_id` 解析失败 = 库里数据异常，直接 403 无日志，数据腐化不可见 | 返回前 `tracing::warn!(artifact_id = row.id, ...)` | safe |
| artifact_api.rs:273-295 | `sandbox_headers` 每次响应新建 7 个 HeaderValue 的 HeaderMap，内容全静态 | `static LazyLock<HeaderMap>` + `.clone()` | safe |
| artifact_api.rs:314-319 | `encoded_download_name_ext` 逐字节 `format!("%{byte:02X}")`，每个字节一次堆分配；313 行还先整串 `format!("{title}.{ext}")` 再遍历 | 用 `write!(out, ...)`；分段迭代 title/ext 字节省掉整串拼接 | safe |
| artifact_api.rs:402-407,449-455 | export/promote 构造 `ViewQuery` 时塞了 `version` 字段，但 `load_versioned` 只读独立 `version` 参数（205-207 行签名），`vq.version` 是死赋值 | 删掉死赋值或让 load_versioned 改读 `q.version` | safe |
| artifact_api.rs:413 | `fmt` 大小写敏感，`CSV`/`XLSX` 被 400；低代码平台常传大写 | `q.fmt.as_deref().map(str::to_ascii_lowercase)` 后比较 | test |
| artifact_api.rs:505,1380 | `find("<table")` 无边界闸，`<tablex>` 之类伪标签也被当表格起点；与同文件 557 行 `extract_cells` 的边界检查不一致 | 补与 557 行同款的 boundary matches! 检查 | safe |
| artifact_api.rs:524,1359,1426 | `find("<tr")` 同样无边界闸（三处），`<trxyz` 会被当行起点 | 同上补边界检查 | safe |
| artifact_api.rs:575-580 | `cell_text` 把 `<br>` 整标签剥掉不留空白，`"a<br>b"` 导出成 `"ab"` 粘连 | 剥标签前先 `replace("<br>", " ")`（大小写不敏感） | test |
| artifact_api.rs:587-595 | `decode_entities` 串 7 次全文 `replace`，每次新分配一个 String | 单趟扫描 `&` 起跳一次解码（保持 `&amp;` 语义：左到右天然等价） | safe |
| artifact_api.rs:676-686 | `worksheet_xml` 每行每格 `push_str(&format!(...))`，2 万行×N 列 = 数万次临时 String | 改用 `write!(out, ...)` 直写缓冲 | safe |
| artifact_api.rs:697,699,711,735-746 | `as u32`/`as u16` 静默截断：单部件 >4GiB 或部件名 >65535 字节时 ZIP 头写错值而无任何报错 | 写前 `debug_assert!`/`assert!` 尺寸上限（内容全在内存，失败即 panic 可接受） | safe |
| artifact_api.rs:901 | `q.conv_id.clone().unwrap_or_default()` 无谓 clone 一个 Option<String> | `.bind(q.conv_id.as_deref().unwrap_or(""))` | safe |
| artifact_api.rs:921 | `starts_with("<h1")` 大小写敏感且无前缀边界：`<H1>` 不命中会叠双标题，`<h1foo` 误命中会吞标题 | 小写化后判 `<h1>`/`<h1 ` 两种边界 | safe |
| artifact_api.rs:1029,1057,1081,1168,1186,1211,1238,1292,1323,1356,1379,1471 | 12 处清洗循环都在**每轮迭代内**对整串重算 `to_ascii_lowercase()`，命中多则 O(n×次数） 反复全量分配 | 每轮循环顶算一次、或改成「替换后从变更点续扫」并注释说明现状取舍 | safe |
| artifact_api.rs:1018-1020 | `find_ascii_case_insensitive` 每次调用把 needle（全是 ASCII 常量）也小写化一遍，且 haystack 整串复制 | needle 入参约定已小写（改签名注释），只小写 haystack | safe |
| artifact_api.rs:1247-1250 | class 匹配只认 `class="..."` 紧排写法：`class = "sqlx"`（等号带空格）与无引号写法静默漏过 | `class` 属性解析放宽等号两侧空白 | safe |
| artifact_api.rs:1266-1271 | `matching_element_end` 的开标签 needle `<{tag}` 无边界闸，`<tablex>` 被计作 `<table>` 嵌套层，深度算错导致结束位偏移 | open needle 命中后加同款 boundary 检查 | safe |
| artifact_api.rs:1306,1411,1480 | `term.to_ascii_lowercase()` 在逐元素/逐单元格的 `any()` 闭包里反复分配（调用处的 terms 本来就全是小写字面量） | 循环外一次性 lower 成 `Vec<String>`，或签名约定 terms 已小写 | safe |
| artifact_api.rs:1321-1325 | `remove_heading_section` 只认裸 `<h2>`/`<h3>`，带属性（`<h2 class="x">`）的该删标题静默漏过 | 开标签匹配放宽到 `<h2>`/`<h2 ` 边界再截到 `>` | safe |
| artifact_api.rs:154,426,887 | 400 文案原样回显 `kind`/`fmt`/`feed` 入参，均为无长度上限的用户字符串 | 回显前 `chars().take(64)` | test |
| artifact_api.rs:789-824,446-479 | share/unshare/promote 三个安全敏感端点成功路径零日志，无审计轨迹 | 各加一行 `tracing::info!`（id、操作人、目标 conv） | safe |
| artifact_api.rs:794 | 已有 token 时（CASE 保留旧值）仍白生成一个 uuid 再丢弃 | 先 SELECT 现有 token，空才生成（或注释说明省一次查询的取舍） | safe |
| artifact_api.rs:372-375 | `chain.iter()` 后 `json!` 克隆每行的 `created_at` String | `into_iter()` move | safe |
| artifact_api.rs:1007 vs 982 | title 里「证据」被**删除**（`replace("证据","")`），body 里被**改名**（`replace("证据","数据")`），同一词两种处置无注释解释 | 注释说明差异，或统一策略 | test |
| artifact_api.rs:496,519-536 | `MAX_EXPORT_ROWS` 只限行数不限单元格数：2 万行×超宽表会在内存里拼出巨型 CSV/XML | 再加一个总单元格数护栏（如 rows×cols ≤ 200k） | test |
| artifact_api.rs:647-655 | `col_letter(0)` 返回空串不报错，调用方恒传 `ci+1` 才安全，契约靠调用方自觉 | 函数首行 `debug_assert!(n > 0)` | safe |
| artifact_api.rs:1526 | `find("</main>")`/`find("</body>")` 大小写敏感，大写闭合标签时锚点丢失退化为追加到文末 | 走 `find_ascii_case_insensitive` | safe |

## settings_api.rs（32 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| settings_api.rs:69 | `serde_json::from_value(v.clone())` 为做校验深克隆整份 settings Value（含全部密文/目录字符串） | 改用 `crate::db::Settings::deserialize(&v)`，校验零克隆 | safe |
| settings_api.rs:94-95 | `std::fs::write` 同步阻塞 IO 跑在 async handler 链上（persist_settings 由全部写端点异步调用） | 换 `tokio::fs::write` 或 `spawn_blocking`，行为一致 | test |
| settings_api.rs:96 | 恢复失败 error 日志无 `path` 字段，排障时不知道哪个文件写坏 | 日志加 `path = %prepared.path` | safe |
| settings_api.rs:101 | `.expect("cfg 锁中毒")` 锁中毒直接 panic 请求任务；此处是整体覆盖写，中毒值无影响 | `unwrap_or_else(std::sync::PoisonError::into_inner)` | test |
| settings_api.rs:113-117 | `valid_name` 对 `name.chars()` 走两遍（count + all） | 单遍 fold 同时计长与校验字符 | safe |
| settings_api.rs:140-143 | `configured_thinking_level` 每次调用对最多 3 个预设思考体做 `serde_json::from_str`；catalog(L291)/llm_config(admin_api.rs:756,772) 按供应商逐个调用，重复解析静态数据 | 预设体解析一次缓存（OnceCell）或直接比 raw 字符串 | safe |
| settings_api.rs:234,272 | `file_provider_name(&cfg)` 在同一函数算两次（`file_provider` 与 `file_provider_for_delete` 同值） | 复用第一个绑定，删第二个 | safe |
| settings_api.rs:298 | `custom.sort_by` 用大小写敏感的 `as_str().cmp`，而同响应里 `candidate_names`(L304) 按小写排序——两个列表排序口径不一；且依赖 `Option<&str>` 的 Ord | 统一 `sort_by_key( | v |
| settings_api.rs:425,436,441,520,527,540,545 | `put_mysql_target` 内 `st.cfg()` 克隆 6+ 次，每次都是整份 Settings（含解密后明文 DSN/key）的内存复制 | 函数顶部快照一次 `let cfg = st.cfg();` 全程复用 | safe |
| settings_api.rs:452-476 | DMS 探针顺序建 3 次连接（test_pool→connect→admin 查询），同一 DSN | admin 检查复用 L459 的 candidate 池，省一次建池 | test |
| settings_api.rs:483,486,502 | auth 池大小字面量 `5` 重复 3 处（admin_api.rs:935,948 分析池 `10` 同理） | 提 `const AUTH_POOL_SIZE` / `ANALYSIS_POOL_SIZE` | safe |
| settings_api.rs:539-544 | `hot.then( |  | …find…map(…)).flatten()` 嵌套 Option 可读性差 |
| settings_api.rs:520-524 | keep_secret 查找走 `db_targets()`，该函数会过滤「与 DMS 同端点但非显式 production_lookup」的目标（db.rs:725-731），存在的目标报「目标 X 不存在」误导 | 直接查 `cfg.mysql_targets` 做 matching_key | test |
| settings_api.rs:605-617 | del_mysql_target 把 patch 闭包的具体错误（"目标 X 不存在"/"没有 mysql_targets"）一律吞成 500 SETTINGS_WRITE_GUIDANCE | 按错误内容映射 404/400，保留文案 | test |
| settings_api.rs:641-642,915-916 | key 格式校验（长度 8..4096 + 控制字符）在两处逐字复制 | 提 `fn valid_key(&str) -> bool` 共用 | safe |
| settings_api.rs:644,686,992 | `let next = st.cfg()` 把**当前**快照命名成 next（L874 同类变量叫 current_cfg） | 统一改名 `current_cfg` | safe |
| settings_api.rs:737,739 | test_db keep_secret 分支两次 `st.cfg()` 克隆 | 一次快照复用 | safe |
| settings_api.rs:748-752 | test_db 传了已存在目标 `name` 但 `type` 为空 → 400「能力类型只允许…」，其实该目标能力在 cfg 里已知 | type 为空且目标存在时回落已配置 capability | test |
| settings_api.rs:787-798 | test_llm 的 `base_url` 没有 put_llm_provider L887 的 `public_service_url` SSRF 闸，同为 admin 触发出站请求，信任面不一致 | 加同样的 public_url 一致性校验 | test |
| settings_api.rs:890 | `req.thinking` 缺省即 "off"：页面外的客户端编辑供应商时漏传 thinking 会把已配置思考档静默重置；结构体文档（L824-825）未写明「缺省=重置」 | 文档补一句缺省语义（或缺省改 "keep"，属行为变更） | safe |
| settings_api.rs:906-912 | 只填 model_precise 时落盘 `model_fast: ""`；校验（L929-933）用 precise 回填通过，但存的是空串——catalog(L289) 显示空、llm_config(admin_api.rs:770) 回填，两端表示不一致 | 落盘前归一化 `mf = if mf.is_empty() { mp.clone() }` | test |
| settings_api.rs:1011-1013 | `(!restores_builtin).then( |  | matching_key(...)).flatten()` 绕 |
| settings_api.rs:1058-1062 | set_fallback_vision 只验供应商存在，不验 vision 能力/key_ready；失败延迟到 commit 里变成笼统 LLM_CONFIG_GUIDANCE | 提交前预检 `supports_vision && key_ready`，给精确 400 | test |
| settings_api.rs:1117 | `st.cfg().kb_rrf_weights` 为读 4 个 f32 克隆整份 Settings | 只读需要的字段（加轻量访问器或读锁内拷贝权重） | safe |
| settings_api.rs:1118-1123 | 四路全 None 的请求会把现值原样重写一遍（完整文件写+热更），纯无操作 | 全 None 直接返回现值成功，跳过落盘 | test |
| settings_api.rs:1143 | 热更日志只记 `kg`/`ext_kb`，漏 `metadata`/`relation`——四路变更审计不全 | 四个字段全进 info 日志 | safe |
| settings_api.rs:171-172 | commit_llm_settings 把 prepare_settings 的全部失败（坏 JSON/解密失败/RRF 校验）压成 500 SETTINGS_WRITE_GUIDANCE，校验类本应是 400 且具体原因丢失 | 透传校验类错误为 400 + 原文案 | test |
| settings_api.rs:28-33 | `err()`/`ApiErr`/`ApiRes` 与 admin_api.rs:37,61-63 逐字重复；本文件已依赖 admin_api（settings_admin_only） | admin_api 的 err/ApiErr 提 pub(crate)，本文件复用 | safe |
| settings_api.rs:35-48 | 指引文案常量与 admin_api.rs:806-807,837-839,940-941,961-962 的字符串字面量重复 | 共享常量，两处引用 | safe |
| settings_api.rs:188-193 | IdentQuery 文档自称「GET/DELETE 的身份字段」，但 settings_admin_only 完全忽略它们（admin_api.rs:94 `_id`，Bearer-only 是设计） | 注释改写「为签名对称而收，校验只认 Bearer」 | safe |
| settings_api.rs:420,639,872 | 三处名字闸文案不一（一个说「ASCII 字母数字」、一个只说「字母数字」） | 统一文案或共享常量 | safe |
| settings_api.rs:695-696,1000,598 vs admin_api.rs:74-76 | 删除类端点「不存在」状态码不一：settings_api 用 400，admin_api 用 404（affected 纪律） | 统一 404（或明确记录差异理由） | test |

## crates/agent/src/gather.rs（32 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/agent/src/gather.rs:1 | 模块文档仍写「六路召回」，实际召回路已扩到 13 路（指标/维度/术语/few-shot/码值/值域/关联图/源背景/表/经验/术语递归/元素/教训） | 改成「多路召回」或更新计数 | safe |
| crates/agent/src/gather.rs:5-6 | 搬运注释列的是拆分前串行序「表召回 → 指标 → …」，与现行分波序（表召回在波2，:142）直接矛盾，读注释会误判 await 顺序 | 注明这是历史出处顺序，实际顺序见 :38-41 波次注释 | safe |
| crates/agent/src/gather.rs:38-41 | 波次注释只写到波3 且把「元素、对面表卡片」归入波3，但代码在 :177 明确分了波4 | 注释补出波4（元素+对面表卡片），与代码对齐 | safe |
| crates/agent/src/gather.rs:48-49 | `cx.question.to_string()` 出现两次（`seen` 一次、`slice_texts` 首位一次），同一份问句白分配一份 | `let q = cx.question.to_string(); seen.insert(q.clone()); once(q)` 省一次克隆 | safe |
| crates/agent/src/gather.rs:54 | `seen.insert(w.clone())` 对**重复**窗口也先 clone 再 insert 失败，重复片白付一次分配 | 先 `seen.contains(w)` 判重，命中才 clone | safe |
| crates/agent/src/gather.rs:82-83 | 注释说「与上一行同形（match 而不是 `unwrap_or_default`）」，但 :81 用的是 `.map(...)` 不是 match | 改为「与上一行同类（都不进六路 warn 判据）」 | safe |
| crates/agent/src/gather.rs:88-94 | 注释仍称「六路召回失败」「这六行」，实际 `gather` 体内 warn 点已 9 处（多了 ds_background/经验/术语递归） | 更新计数措辞，避免后人按「六」核对 | safe |
| crates/agent/src/gather.rs:101 | `cap_yesterday` 在问句无时间词（`time_predicate` 返 None）时也白算一遍 `.any()` | 挪进 :234 的 `.map` 闭包内惰性计算 | safe |
| crates/agent/src/gather.rs:153-159,511-515 | 「收集 ids → clone pool → spawn bump_hits」整块重复两次 | 抽 `fn spawn_bump_hits(pg: &PgPool, ids: Vec<i64>)` 两处调 | safe |
| crates/agent/src/gather.rs:158,514 | spawn 内 `let _ = ...` 静默吞 bump 失败，与本文件「每一路降级都要吼一声」的纪律不一致（bump 失败连 debug 都没有） | 至少 `if let Err(e) = ... { tracing::debug!(...) }` | safe |
| crates/agent/src/gather.rs:185 | `pitfalls?` 在波4 join! **之后**才判：教训召回失败时，元素召回与对面表卡片查询白跑一轮才返回 Err | 把 `let pitfalls = pitfalls?;` 挪到 :172 之后（波3 判完再发波4） | safe |
| crates/agent/src/gather.rs:193-198 | 对面表 `schema_card` 的 `Err`（PG 读失败）被静默并入 :204 的 `missing`，而 :201 注释声称 `missing` =「meta.table_doc 里没有这张表」——读失败与声明缺口被压成同一类，与 :88-94 的留痕纪律冲突 | Err 分支单独 warn/计数（注意 :1066 的 warn==unwrap_or_default 条数判据要同步调形态） | test |
| crates/agent/src/gather.rs:210 | `counter_cards.join("")` 先拼出一份完整中间串再 push 进 schema，白付一次全量分配 | 循环里 `for c in &counter_cards { schema.push_str(c); }` | safe |
| crates/agent/src/gather.rs:216-224 | `tables_for_rules` 里的 `!v.iter().any( | x | x == t)` 是死判断：`added` 来自 `join_counterparts`，已按**大小写不敏感**排除过召回表；且此处用大小写敏感比较，与 :777 口径不一 |
| crates/agent/src/gather.rs:255 | 日志的 `section_chars(&pc)` 是第三次重算（:305、:424 各算过一次） | `build_context_summary` 返回值里已有 `prompt_chars`，落账前取出复用 | safe |
| crates/agent/src/gather.rs:266 | const 名 `PROMPT_BUDGET_CHARS` 但口径是**字节**（:270 用 `s.len()`，:268 注释也写「字节量」），名实不符 | 改名 `PROMPT_BUDGET_BYTES` 或修 const 文档 | safe |
| crates/agent/src/gather.rs:261-263 | const 文档列的丢弃序缺 :309-313 的「⓪ 经验段先丢」这一步，「绝不丢」清单也只字未提 memories；:860-861 测试注释同样漏 | 文档与测试注释补上 ⓪ 步 | safe |
| crates/agent/src/gather.rs:342 | `ctxs.len() <= 3` 时 `schema_text(&ctxs[..keep])` 重渲出与现值逐字节相同的串（keep==len），纯浪费一次全量分配 | 把重渲挪进 `if ctxs.len() > keep` 块内 | safe |
| crates/agent/src/gather.rs:356-357 | warn 字段 `tables = pc.schema.len()` 记的是 schema **字节数**而不是表数，排查时误导 | 改字段名 `schema_bytes` 或记 `kept_recalled` | safe |
| crates/agent/src/gather.rs:370,713 | `header.split("·v").next()` 剥版本后缀不看后面是不是数字：注册名本身含「·v」（如「新客·vip」）会被静默截断；且 `unwrap_or(header)` 是死分支（split 恒有首项） | 只在 `·v` 后全为数字时剥（正则或 chars 判定），去掉死分支 | test |
| crates/agent/src/gather.rs:363-371,705-718 | `card_name` 与 `prompt_card_has_name` 把同一段头解析（剥【】、剥四类前缀、剥 ·vN）写了两遍，:362 注释自认 | 抽 `fn card_header_name(card: &str) -> Option<&str>`，两处复用 | safe |
| crates/agent/src/gather.rs:716-717 | fallback `card.strip_prefix(name)` 在 `name` 为空串时恒 `Some`，语义模糊（目前 recall_elements 不会返空名，纯防御缺口） | 加 `!name.is_empty() &&` 闸 | safe |
| crates/agent/src/gather.rs:726-730 | 元素去重对每个候选元素把全部 seen 卡头重新解析一遍（O(elems×cards) 次 `prompt_card_has_name`） | 循环外先把 seen 卡解析成 `HashSet<String>` 名字集，候选直接查 | safe |
| crates/agent/src/gather.rs:506-509 | 回炉的 `recall_memories` 只依赖 :467 已算好的 `qvec`，却串行等在 :472 的 join! 之后，每轮回炉白付一次 PG 往返 | 挪进 :472 的 `tokio::join!`（降级语义不变，:1004 的 join! 判据仍绿） | safe |
| crates/agent/src/gather.rs:518,628,748 | `push_str(&format!(...))` 先分配临时 String 再拷贝（三处同型） | `use std::fmt::Write; write!(out, ...)` 或拆成两次 push_str | safe |
| crates/agent/src/gather.rs:594-602 | 预算循环 `[20, 8]`：当 `dl.len() <= 8` 时两轮压入**完全相同**的内容，第二轮是纯重算；且首轮 20 档即压下时静默丢弃 `dl.len()-20` 行无任何痕迹 | 循环内 `if keep >= dl.len()` 时一轮即定；截断开火时补一条 info | safe |
| crates/agent/src/gather.rs:603 | warn 文案「维度段已砍到 8 行仍超」在 `dl` 为空（schema+指标段独自超预算）时也照发，误导排查方向 | 日志带上 `dl_len`/实际 keep 行数 | safe |
| crates/agent/src/gather.rs:618-620 | `requote` 在 MySQL 恒等路径（今天唯一路径）仍 `to_string()` 全量分配，每条指标/维度行各一次 | 返回 `Cow<'_, str>`：恒等路径借、换引号路径拥有 | safe |
| crates/agent/src/gather.rs:619 | `s.replace('`', quote)` 无差别替换：字符串字面量里的反引号（如 `WHEN 'it`s' THEN`）也会被换，接 PG 源那天会改坏含反引号的字面值 | 至少注释声明「声明里字面值不许含反引号」，或做引号感知替换 | test |
| crates/agent/src/gather.rs:63,168,540 | `limit: 6` 魔法数三处（:534 注释专门记了这个耦合） | 提 `const RECALL_LIMIT: usize = 6;` | safe |
| crates/agent/src/gather.rs:144,506 | 经验召回 limit `3` 魔法数两处，改了其中一处另一处不会跟着变 | 提 `const MEMORY_LIMIT: usize = 3;` | safe |
| crates/agent/src/gather.rs:999 | 防恒真阈值 `body.len() < 3600` 是字节数且函数一变长就要手抬（注释自认），属于脆弱判据 | 可换成「body 不含下一个 `fn ` 签名」这类结构性判据 | safe |

## mod.rs（32 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| mod.rs:1,3 | 「六种召回」过时：本模块 re-export 的召回函数已 11 个（表/指标/维度/术语/术语递归/码值/值域/元素/教训/ODS 候选/JOIN 证据） | 改计数或改写成「各召回族」不写死数字 | safe |
| mod.rs:3-5 | 「六种召回今天在 `server/src/meta.rs` 各带…形参」「调用点全在 `pipeline::generate_sql` 一处」——现在时措辞双双过时：函数已搬入本模块；实际调用点是 agent/gather.rs、agent/triage.rs:211、server/corrector.rs:559、server/direct.rs:2969,3065、server/main.rs:951 | 改过去式（「搬运前…」）或更新调用点清单 | safe |
| mod.rs:33 | 「`limit` 的语义随召回族不同」没写另一半事实：指标/维度/术语/码值/值域召回**压根不读** limit（gather 波 1 塞的 `limit: 6` 是纯占位，gather.rs:63） | 注释补「仅 retrieve/elements/pitfalls 读」 | safe |
| mod.rs:40 | 字段注释「条数上限 / 表召回的 k」同上行漏「多数召回族不读此字段」 | 同上补一句 | safe |
| mod.rs:44-45 | embed 文档没写优先级：`embed_slices` 非空时 `embed` 字段被 `recall_elements` 完全忽略（cards.rs:332-338 的 if/else） | 写明「embed_slices 优先，embed 仅在切片为空时兜底」 | safe |
| mod.rs:44 | 「`None` = embed 缺席 → 向量路降级跳过」：跳过是静默的（schema.rs:99-101、cards.rs:337 均无日志），「embed 挂了」与「没开向量」观测不可分 | 两处降级点各加一次 `debug!` | safe |
| mod.rs:46-49 | embed_slices 契约「整句在首位」对行为无影响（MIN 距离与顺序无关），属多余承诺；真正影响行为的是「含不含整句」却没写成判据 | 注释改写为「需含整句向量，顺序无关」 | safe |
| mod.rs:49 vs 45 | `embed_slices: &[String]` 与 `embed: Option<&str>` 形态不对称，迫使实现侧 `to_vec()` 全片克隆（cards.rs:333，每片是几百字符向量字面量） | 统一为 `&[String]`（空=缺席），省一次克隆；调用点 gather.rs:141 / main.rs:949 / cards.rs:335 同步 | safe |
| mod.rs:36 | 「别做归一化——顺序即行为」：归一化改的是内容不是顺序，「顺序即行为」与上半句不搭 | 改「逐字 contains 即行为」 | safe |
| mod.rs:32,48 | 两处「只有 X 读它」靠人肉维护，新加读者不会编译红 | 注释补 grep 锚（字段名+「读者清单以此为准」） | safe |
| mod.rs:12-13 | 「与今天 `embed_query()` 返 `None` 时的降级等价」——`embed_query` 今在 connector/agent 侧（gather.rs:70），「今天」式指代已漂 | 改成指向 gather 的现行表述 | safe |
| mod.rs:28-31 | `ds_pred_at` 两次 `replace` 全串扫描 + `format!("${n}")` 临时分配，每次调三次堆分配 | 单次 `format!` 拼装或 `format_args!` | safe |
| mod.rs:133-136 | `scoped_asset_pred` 与 `ds_pred_at`(28-31) 函数体逐字重复 | 抽公共内部 fn，两处各留一行 | safe |
| mod.rs:47/91/107 vs 274-280 | SQL 正则里的 7 类表前缀与 Rust `source_refs` 的前缀清单是两份手写拷贝，漂移无守卫 | 注释互指，或抽共享 const 片段 | safe |
| mod.rs:162-167 | `warehouse_table_parts` 对三段名 `a.b.c` 取 database=`a.b`，而 `source_refs`:272-281 取 `b`，三段输入两处口径不一 | 统一 `rsplitn(2,'.')` 语义或注释钉住 | test |
| mod.rs:171-173 | `warehouse_asset` 每次线性扫 57 项 `ASSETS`，且在 `source_uses_warehouse_catalog`:291、`catalog_allows_*` 循环内被反复调用 | `LazyLock<HashMap>` 静态索引 | safe |
| mod.rs:186-194,200 | `push_warehouse_ident`→`warehouse_qualified_table` 对每个 ident 各 `format!` 一次临时 String | 直接写入 `out` 或返回 `Cow` | safe |
| mod.rs:268-269 | `source_refs` 两次 `replace` 两遍全串扫描 | 合并为一次 chars 过滤 | safe |
| mod.rs:266-284 | `source_refs` 不剥单引号字符串字面量，字面量里的 `t_x` 会被当表引用（fail-closed 误拒声明） | 复用 `warehouse_qualified_source` 的引号跳过逻辑 | test |
| mod.rs:318,336 | `catalog_allows_column`/`forbidden_default_sales_column` 每次调用先 `to_ascii_lowercase()` 分配再 `matches!` | 改 `iter().any(\ | c\ |
| mod.rs:371 | `return ds != datasource::DMS_DS_ID;` 把布尔判据当返回值，读三遍才能确认语义 | if/else 显式返回 true/false | safe |
| mod.rs:414, exemplar.rs:73 | `.replace("sf.", "")` 会误伤含 `sf.` 子串的标识符（如 `asf.qty`→`a.qty`）致合同比对假阴性 | 只剥前缀位置的 `sf.`（`strip_prefix`/分词后比） | test |
| mod.rs:493 | `database.as_deref() == Some("sales_dw")` 大小写敏感，与 384 行 `eq_ignore_ascii_case("sales_dw")` 不一致（今天靠 `source_refs`:281 已小写化才凑巧成立） | 统一 `eq_ignore_ascii_case` | safe |
| mod.rs:476-479 | `catalog_allows_metric_dimension` 硬编码 8 个中文维度名，与 389-399 的 `Dimension` 枚举两份真相 | 从枚举 `name()` 派生该清单 | test |
| mod.rs:504 | `is_backup_table` 收集 `tail` 字符串却只用其长度 | `chars().rev().take_while(..).count()` | safe |
| mod.rs:516-518 | 两遍 `split('_').any(..)` 分别判 6/8 位数字段 | 单遍 `matches!(seg.len(), 6 \ | 8)` |
| mod.rs:534,538 | `domain_of`：`t_market` 前缀先于 `t_marketing` 命中，`t_marketing_goods`/`t_marketing_zone_product`（seed.rs:90/105 真实存在）永远归「市场费用」，("t_marketing","营销") 是死分支 | 长前缀排前（或精确化前缀）并补断言 | test |
| mod.rs:552-554 | `extract_tables` 闭包里 `contains(&cur.to_string())` + `push(cur.to_string())` 双分配 | `tabs.iter().any(\ | t\ |
| mod.rs:552 | `starts_with("t_")` 大小写敏感，LLM 大写 SQL 的 `T_SALES_ORDER` 锚定漏抓 | `to_ascii_lowercase` 后判或大小写无关前缀 | test |
| mod.rs:548-566 | `extract_tables` 只认 `t_` 前缀，与 `source_refs`(274-280) 的 7 类前缀口径不一，dws_/ads_ 锚定全漏 | 复用同一前缀清单 | test |
| mod.rs:526-527 | `is_sensitive_col` 每列每次 `to_lowercase()` 分配（schema 渲染热路径逐列调） | 调用侧一次小写化传入，或 `SENSITIVE_COLS` 预存 | safe |
| mod.rs:3 | 注释「`gate`…届时在此加 `pub mod gate;`」已过期——gate 已存在（mod.rs:9） | 删掉那句待办式注释 | safe |

## web/src/BiChart.vue（30 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| web/src/BiChart.vue:19,104,115 | 文件混入孤 `\r` 行尾（Read 显示裸 CR），与同文件 LF 行混杂，diff/补丁工具易翻车 | 统一为 LF（或 CRLF），一次性格式化 | safe |
| web/src/BiChart.vue:27 | 注释「普通卡片保持原来的 340px」与代码不符：L35/L71 实际是 330 | 注释改为 330（或标注 330/286 两档） | safe |
| web/src/BiChart.vue:35,71 | 默认高 330 写死两处（`ref(props.height ?? 330)` 与 `syncHeight` 兜底），改一处忘另一处 | 提 `const DEFAULT_HEIGHT = 330` | safe |
| web/src/BiChart.vue:67,71,223,260,264 | 魔数散布：compact 阈值 560、compact 高 286、maxLabels 6/12、rotate 38/28、label width 72/110，均无命名常量 | 集中到文件头常量区并各加半行注释 | safe |
| web/src/BiChart.vue:84-87 | `isGrossMarginLabel` 同一份逻辑复制三处（另两处 App.vue:1957-1959、ResultPanel.vue:276-279），改判据时极易漏一处 | 移到 format.ts 导出共用 | safe |
| web/src/BiChart.vue:86 | 只做全等匹配「毛利率/销售毛利率」，`平均毛利率`、`毛利率(%)`、`毛利率（净）` 等真实变体漏判 → ratio 不 ×100，图上 0.13 显示成 0.1% | 改为「包含毛利率」匹配或正则，补测试 | test |
| web/src/BiChart.vue:89-94 | 0~1 ratio ×100 的合同只覆盖毛利率；其它 percent 语义列若后端给 ratio（如「税率」），图表与表格会同时差 100 倍而无任何告警 | 注释里点明该合同仅毛利率，或按 semantic==='percent' + 值域 ≤1 启发式处理（带测试） | test |
| web/src/BiChart.vue:105-108 | 注释整段描述「原来有两个死函数」的考古信息，属 commit message 内容，常驻源码干扰阅读 | 精简为一行「多序列逻辑见下方 groups.map」 | safe |
| web/src/BiChart.vue:113 | y 语义兜底 `inferred === 'none' ? 'count'`：未识别的金额列（如「GMV」「营收」）被当 count，丢 ¥ 符号静默降级 | 与 format.ts:34 的 money 词表扩充联动（见下），或兜底时保留 'none' 不压缩 | test |
| web/src/BiChart.vue:121-125 | `props.y[0]` 全程不防空数组：y=[] 时 sort/轴/series 全部静默产出空图，无任何兜底 | render 开头 `if (!props.rows.length |  |
| web/src/BiChart.vue:124 | TOP 排序比较器每次比较调 2 次 `metricNumber`（toNum+正则+列名查表），O(n log n) 倍重复计算 | 先 `map` 出 `number[]` 再按预计算值排序 | safe |
| web/src/BiChart.vue:124 | TOP 排序把 null 当 0：含负数的指标里 null 排在真实负数前面，「取前 N」会把缺数据行顶进榜单 | 比较器里 null 显式沉底（`?? -Infinity` 或单独判空） | test |
| web/src/BiChart.vue:133 | 饼图上色排序与 L124 TOP 排序是同一个比较器的第三份拷贝 | 提 `byValueDesc(yi)` helper 共用 | safe |
| web/src/BiChart.vue:134 | `Math.min(rank, len-1)` 截断：第 6 名起所有扇区共用同一个最浅色，7+ 类的饼图尾部一片同色无法区分 | 改为取模循环色阶（榜首仍最深），或超出部分回落到 `theme.series` | test |
| web/src/BiChart.vue:137 | `color: theme.mono` 调色板与 L162 每个 datum 的 `itemStyle.color` 完全重复，调色板永生效不到 | 删掉 `color` 键 | safe |
| web/src/BiChart.vue:147,165 | 边界重叠：legend 条件 `>5`、label 条件 `<=6`，恰好 6 类时图例和扇区标签同时显示，信息重复拥挤 | 统一阈值（如 legend `>=6` 配 label `<=5`），消掉交集 | test |
| web/src/BiChart.vue:147,158 | `dataIdx.length > 5 \ | \ | compact` 同一表达式写两遍，改阈值要改两处 |
| web/src/BiChart.vue:165,171 | `!compact && dataIdx.length <= 6` 同样写两遍（label 与 labelLine） | 提 `const showLabels = ...` | safe |
| web/src/BiChart.vue:159-160 | 饼图 `name: fmt(...)`：两个不同原始值格式化成同一字符串（如未登记区划码原样输出撞名）时，echarts 图例联动会把两扇区当一项 | name 撞车时拼下标去重，或在注释中声明依赖后端维度唯一 | test |
| web/src/BiChart.vue:195,203 | 分组键用**格式化后**的字符串：两个原始类别格式化成同串会静默合并；同 (g,x) 重复行 `cellOf.set` 后者覆盖前者，无告警 | 注释声明「依赖后端分组唯一」或在重复时 console.warn | test |
| web/src/BiChart.vue:217 | `const yAxis: any` 丢掉类型，后续拼错键名（如 `axisLable`）编译器不拦 | 用 echarts 的 `YAXisComponentOption` 类型 | safe |
| web/src/BiChart.vue:224 | `labelInterval` 粒度跳变：13 个标签（仅超 1 个）就算出 interval=1，直接砍掉一半标签只显示 ~7 个 | 超出不多时仅靠 rotate+hideOverlap，或 interval 公式取 `floor` 策略，带视觉回归 | test |
| web/src/BiChart.vue:271-304 | 分组/单序列两个 series 分支重复铺 `barMaxWidth/barGap/smooth/showSymbol/symbolSize/emphasis` 六七个键 | 提公共 base 对象再各自展开 | safe |
| web/src/BiChart.vue:278,297 | `smooth: props.kind === 'line' ? .24 : false` —— bar 系列挂 `smooth:false`、`symbolSize:6` 等无关键，纯噪音 | line 专属键用条件展开（`...(isLine && {...})`） | safe |
| web/src/BiChart.vue:301 | 数据标签只在**非分组**单序列分支有；后端从单序列切到多序列（即便只切出 1 组）后柱顶数字标签静默消失，UX 不一致 | 分组且 `groups.length===1` 时同样给 label（带测试） | test |
| web/src/BiChart.vue:277 | `SERIES[si % SERIES.length]` 取模回绕：第 9 组与第 1 组同色且图例相邻，无法区分 | 超 8 组时注释说明取舍，或拼第二组偏移色 | safe |
| web/src/BiChart.vue:50-64,118 | `themeTokens()` 每次 render 调 8 次 `getComputedStyle`；render 由 watch 频繁触发 | 主题翻转（MutationObserver）时缓存一份 tokens，render 直接用 | safe |
| web/src/BiChart.vue:309-317 | `resize()` 里改 `chartHeight` → 自身高度变化再次触发 ResizeObserver → 白跑一轮 `syncHeight+resize`（虽不死循环） | 高度未变时跳过后续调用，或 RO 回调里比对 clientWidth 是否真变 | safe |
| web/src/BiChart.vue:337-341 | `watch(..., { deep: true })` 对 `props.rows`（可达数百行）做全量深遍历；而父级（App/ResultPanel）都是整体替换 rows 引用，深比较买的是用不到的能力 | 去掉 `deep`（依赖引用替换语义），带图表刷新回归测试 | test |
| web/src/BiChart.vue:345 | `aria-label="业务数据图表"` 静态文案：同页多张图（同窗补充、深度页多 section）读屏无法区分是哪张 | 绑定动态 label（kind + y 列名，如「柱状图：销售额」） | safe |

## corrector.rs（30 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| corrector.rs:90-96 | `real_tables` 为空（如 `SELECT 1`）时仍白跑一次 `meta.table_doc` 查询；后续分支必然走到 `Ok(None)` | 在 `collect` 之后加 `if real_tables.is_empty() { return Ok(None) }`，语义逐字等价 | safe |
| corrector.rs:99-102 | `missing` 判定是 O（表数×已知名单） 的双重线性扫描，且每对都跑 `eq_ignore_ascii_case` | 先建 `HashSet<String>`（已知名单已 lower）一次，`real_tables` 逐项 `contains` | safe |
| corrector.rs:113 | `known_tables` 来自 `SELECT lower(table_name)`，已是小写，`t.to_ascii_lowercase()` 是每表一次冗余分配 | 直接用 `t.as_str()` 作 hay | safe |
| corrector.rs:115-117 | `part.to_ascii_lowercase()` 对每表×每词元都分配新 String；`real_tables` 出自 AST 已小写 | 预先一次性 lower 或直接 `hay.contains(part)` | safe |
| corrector.rs:120,124 | 魔法数 20 出现两次（候选表截断） | 提常量 `const TABLE_HINT_CAP: usize = 20;` | safe |
| corrector.rs:149-154 | `table_cols` 对每表 `cols.clone()` 深拷贝整个 HashSet；`grouped` 之后不再使用 | 改用 `grouped.remove(t)` 移动语义，零克隆 | safe |
| corrector.rs:165-166 | `seen.insert((table.clone(), col.clone()))` 与 `bad.push((table.clone(), col.clone()))` 同一对字符串克隆两次 | 先组一次 `(String,String)`，clone 一份给 seen、原值进 bad | safe |
| corrector.rs:213-214 | `resolve_key` 对每个访问到的二元/IN 表达式都 `to_lowercase()` 两次分配 | 先按原值查 `aliases`，不中再 lower 查一次（别名表本就小写时可直接 lower 一次后复用） | safe |
| corrector.rs:237-248 | `col_side` 与 `lit_side_is_right` 恒相等（`(true,true)`/`(false,false)`），且 `col_side` 被 `let _ =` 丢弃——死变量 | 折叠为单个 `lit_side_is_right: bool` 计算，删掉死绑定 | safe |
| corrector.rs:310-313 | `link_values_with` 先 `parse_sql` 再查 `maps.is_empty()`；码表为空时白解析一次 | 把 `maps.is_empty()` 判断提到解析之前 | safe |
| corrector.rs:371-372 与 546 | 两处 `OPT_OUT` 常量同构分散，今后加词容易只改一边 | 提为模块级共享常量（或同文件相邻定义并互注） | safe |
| corrector.rs:399 | `*prev != (func.clone(), distinct)` 在每次比较里都 `func.clone()` 分配 | 改 `prev.0 != func \ | \ |
| corrector.rs:433 | `name.0.last()?` 在 for 循环里用 `?`，空表名会让整个 `locate_target` 提前返回 None 而非跳过该项（实际不可达，但读法误导） | 改 `let Some(t) = name.0.last().map(...) else { continue };` 显式跳过 | safe |
| corrector.rs:480 | `scope_filter.to_uppercase().contains("SELECT")`：整串分配 + 子串误伤（列名含 `selected` 之类即整条口径被跳过，静默不补） | 用 `split_whitespace` 找独立 `SELECT` 词元，或大小写不敏感的词边界匹配 | test |
| corrector.rs:559-567,584-589 | 每个命中指标/表级口径都 `add_scope_filter(&cur,…)` 全量重 parse 一次 SQL，N 个命中 = N 次解析 | 循环外解析一次、循环内对同一 AST 累积修改（或至少注释说明命中数 ≤ 个位数、量级无害） | safe |
| corrector.rs:610 | `pub fn fix_group_by(...) -> Option<String> {    use sqlparser::ast::{` —— `use` 挤在签名同行，全仓孤例 | 正常换行（rustfmt 会收） | safe |
| corrector.rs:638-649 | `match item` 三个分支只可能产出 `Some`，随后 `if let Some(e) = expr` 是必然成立的冗余层 | match 直接绑定 `e`（`_ => return None` 保留） | safe |
| corrector.rs:685 | `top_select` 里 `matches!` 判定写成一行 200+ 字符，难读 | 抽 `fn is_expr_item(&SelectItem) -> bool` 或折行 | safe |
| corrector.rs:778 注释 vs 780-804 实现 | 注释说「沿 WHERE 的 **AND 链**…收集」，但 `walk` 对**所有** `BinaryOp`（含 `Or`）递归——`A OR B` 两个分支的时间上界都会被算成顶层约束 | 要么只在 `B::And` 时下钻（行为改动带测试），要么改注释承认全树扫描 | test |
| corrector.rs:796-802 | `Between`/`InList` 分支只认 `Expr::Identifier`；`o.order_time BETWEEN …`（CompoundIdentifier）不登记下界——虽最终因无上界记录而整体不动，但 `o.t BETWEEN … AND other_time < x` 混合形态会对 other_time 误补 | `Between`/`InList` 也接受 CompoundIdentifier 取末段（与 782-787 同形） | test |
| corrector.rs:821 | 补下界用裸 `Identifier(c)` 丢了原限定符：多表 JOIN 下追加的 `order_time >= '1970-01-01'` 可能撞 MySQL 1052 歧义 | 收集时保留完整前缀（`o.order_time`），补回时也带前缀 | test |
| corrector.rs:826-829 | 追加条件不包 `Nested`：`A OR B` 顶层时 `A OR B AND lb` 重解析成 `A OR (B AND lb)`，语义被换（`add_scope_filter` 在 524-531 恰恰包了 Nested，两处不一致） | 左操作数是 `Or` 时包 `Expr::Nested`，与 524-531 对齐 | test |
| corrector.rs:777 | `ish` 子串匹配假阳：`candidate`、`update_date` 之外的 `menddate` 类列名含 `date` 即被当时间列 | 至少注释承认已知假阳；或改后缀/词边界匹配（行为改动带测试） | test |
| corrector.rs:835 | `fn expr_has_agg(...) -> bool {    use sqlparser::ast::Expr;` 同行 use，同 610 | 换行 | safe |
| corrector.rs:836 | `AGG` 含 `group_concat`，但 `collect_agg_rules`（899）只收五个函数——两清单并存无注释说明差异 | 在 836 或 899 加一行注释说明「判定用名单 ≠ 归一用名单」 | safe |
| corrector.rs:905 | `out.push((name.clone(), …))` 后 `name` 不再使用，clone 多余 | 直接 move `name` | safe |
| corrector.rs:1003 | `.map(\ | c\ | c == col).unwrap_or(false)` 可读性差 |
| corrector.rs:1041 与 1071-1076 | `node_name` 与 `node_func` 是同一表达式 `f.name.0.last().map(to_lowercase).unwrap_or_default()` 算两遍 | 删 1071-1076，复用 `node_name` | safe |
| corrector.rs:1055-1057 | `COUNT(*)` 分支为判「恰一条」先 `collect::<Vec<_>>()` 分配 | 用 `filter(...)` + `next()`/`next().is_none()` 判唯一，零分配 | safe |
| corrector.rs:1090 | `occupied.contains(&(rule.0.clone(), rule.1.clone()))` 每次检查克隆两个 String | `occupied` 改为 `HashSet<(&str,&str)>` 存规则引用 | safe |

## crates/agent/src/answerers/entity.rs（30 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/agent/src/answerers/entity.rs:48-55 | ANALYSIS_TAILS 含被短尾覆盖的死条目：「的销售额」⊂「销售额」、「的销量」⊂「销量」、「的销售量」⊂「销售量」、「的毛利额」⊂「毛利额」、「的毛利率」⊂「毛利率」、「的订单数」⊂「订单数」、「的订单量」⊂「订单量」、「买过的客户」⊂「的客户」（ends_with 语义） | 删死条目（行为逐字节不变） | safe |
| crates/agent/src/answerers/entity.rs:129-171 | ENTITY_PREFIXES「同族最长在前」只有一句注释守着，加错位置无测试会红 | 加一条同族前缀长度降序的自检断言 | test |
| crates/agent/src/answerers/entity.rs:262-263 | `value.chars().count()` 连算两遍（<2 与 >80） | `let n = value.chars().count();` 复用 | safe |
| crates/agent/src/answerers/entity.rs:265 | 非法字符集缺 `;` 与控制字符；business_lookup.rs:499-505 同款校验两者都拒——同仓两条实体链校验口径不一 | 对齐 business_lookup 的字符集，配测试 | test |
| crates/agent/src/answerers/entity.rs:337-341 | `value[..i].chars().last()` 对每个单位命中从头扫一遍 O(i) | 改 `value.as_bytes().get(i.wrapping_sub(1))` 判 ASCII 数字（数字必 ASCII） | safe |
| crates/agent/src/answerers/entity.rs:419-431 | `drop_combo_goods` 先全量 clone 再判空——一个组合都没有时白克隆整个候选 Vec | 先 `iter().all(is_combo)` 早退，否则 `into_iter().filter` 零克隆 | safe |
| crates/agent/src/answerers/entity.rs:426 | 组合判据 `name.contains('：')` 全角冒号一刀切——正常商品名带全角冒号（如「联名款：XX」）会被误剔 | 全角冒号也要求尾巴全数字才判组合，配测试 | test |
| crates/agent/src/answerers/entity.rs:446,453 | fetch_rows 两条 warn 用行内 format（`"回落: {e}"`）且无 sql 字段；hits.rs:133 同语义 warn 是结构化字段+sql——日志形状不一致 | 改结构化 `tracing::warn!(err=%e, sql=%sql, ...)` | safe |
| crates/agent/src/answerers/entity.rs:503-508,539-555 | `push_sales_kpis` 每个 KPI 调 `period_label` → `time_phrase_of(question)` 重解析一遍问句（一次卡片 6 遍） | 函数入口算一次 `time_phrase_of` 传下去 | safe |
| crates/agent/src/answerers/entity.rs:581,586 | accept 与 answer 各跑一次完整 `parse_entity`（category 路径第三跑在 category.rs:14）——词法门本就为省 IO，重复 CPU 解析没注释交代 | 在 accept 注释里补「answer 复跑是 Router 形状要求，同 graph.rs:81-83」 | safe |
| crates/agent/src/answerers/entity.rs:603 | `role_code == "admin"` 大小写敏感；同仓 business_lookup.rs:350 用 `eq_ignore_ascii_case`——权限判据大小写口径不一（权限判据本身不动，仅对齐大小写） | 改 `eq_ignore_ascii_case("admin")`，配测试 | test |
| crates/agent/src/answerers/entity.rs:665 | dedup key `(kind, code.clone(), name.clone())` 每候选克隆两个 String；candidates 生命周期足够借引用做 key | `HashSet<(Kind, &str, &str)>` 借用 key | safe |
| crates/agent/src/answerers/entity.rs:727-729 | 门店候选在 `t_sales_order` 上 `shop_name LIKE '%..%'` + DISTINCT——订单表全表扫，实体卡最重的一条候选 SQL，无注释交代成本 | 注释交代「无门店主档表，实测成本可接受」或后续引门店维度表 | safe |
| crates/agent/src/answerers/entity.rs:754 | LIKE 值只经 esc（单引号）：`_` 通配符未转义（同 category.rs:18） | esc 后追加 `_`→`\_` 转义，配测试 | test |
| crates/agent/src/answerers/entity.rs:773 | `(Employee, Code)` 条件列是 `CAST(e.employee_id AS CHAR)`——WHERE 包列函数，employee_id 索引失效，员工目录全表扫 | 条件改 `e.employee_id = '{safe}'`（值已限数字形时）或注释成本，配测试 | test |
| crates/agent/src/answerers/entity.rs:775 | `(Kind::Category, _) => &[]` 会 join 出空条件串 `AND ()`——今天靠 735 行早退兜底，无人守 | 改 `unreachable!("Category 在 candidates_for 入口已分流")` 或 debug_assert | safe |
| crates/agent/src/answerers/entity.rs:830,862-868 | `employee_denied` 复用 candidate_card → AskResult.sql 是「实体候选匹配：员工目录（精确优先，未自动选择）」——一张**拒绝卡**顶着「候选匹配」的展示 SQL，日志/前端文案误导 | denied 卡单独设 `sql = "员工目录访问被拒（权限不足）"`，配测试 | test |
| crates/agent/src/answerers/entity.rs:871-873 | `esc` 只转义单引号不转义反斜杠：库里来的值（品牌 878、分类 category.rs:82）带 `\` 时在 MySQL 默认 sql_mode 下是转义符，LIKE 模式被悄悄改写（非注入，纯匹配错） | esc 追加 `\`→`\\`（确认 Doris/MySQL 双方言一致后），配测试 | test |
| crates/agent/src/answerers/entity.rs:894 | 品牌卡展示 SQL 用 `SELECT …` 省略号，其余卡（935/985/1155/1338）都嵌真实 SQL——「查看 SQL」/query_log 口径不一 | 嵌真实 goods_sql（已在手），配测试 | test |
| crates/agent/src/answerers/entity.rs:917-922,962-967 | shop_card 与 employee_card 的 recent_sql **没带** `{otime}` 时间窗；customer(1048/1055) 与 goods(1212) 都带——「本月」问门店卡时最近订单却是全期的 | 两处 recent_sql 补 `{otime}`，配测试 | test |
| crates/agent/src/answerers/entity.rs:926-927,976-977 | shop/employee 手写 `stats.rows.first().and_then( | r | r.first()).and_then(cell_num).unwrap_or(0.0)` 两遍；本文件 459-472 已有 `num/num_at`，customer(1136-1139) 就在用 |
| crates/agent/src/answerers/entity.rs:1117,1141-1143 | **潜在 bug**：balance 查询零行时 `num()` 给 0.0 → `bal_val = Some(0.0)` → 无信控记录的客户卡上显示「信控余额 0.00」——「没记录」被答成「余额为零」 | 改 `balance.as_ref().and_then( | rs |
| crates/agent/src/answerers/entity.rs:1150-1153 | customer 卡 `skip(2)`：购买商品数/活跃月份数同时进 KPI 块（1138-1139）和 Entity pairs——同卡同一数字出现两次；goods 卡 `skip(4)`（1335）正好对齐它自己的 4 个 KPI，证实 customer 的 skip 数写错 | `skip(4)`，配测试 | test |
| crates/agent/src/answerers/entity.rs:1013,1019 | profile_sql 模板 `AS `城市`{} , ` 与独立 `{}` 行，拼接出「城市 , COALESCE… , 联系人」的双空格/悬空逗号——可读性差（SQL 合法） | 整理模板占位为 `{select_extra}`/`{join_extra}` 命名参数 | safe |
| crates/agent/src/answerers/entity.rs:1292-1293 | `is_device` 依赖「数仓 profile 第 7 列恰好是物料类型」的**位置耦合**，靠 `is_warehouse() &&` 短路防越界——投影加一列就静默读错字段 | 抽 `const WAREHOUSE_MATERIALTYPE_COL: usize = 7;` 并注释与 1175 行投影的对应关系 | safe |
| crates/agent/src/answerers/entity.rs:1310 | `format!("{gcat} · {gbrand}")`：品牌为空时 KPI 标签成「XX · 」拖尾分隔符 | 空品牌时只用 gcat，配测试 | test |
| crates/agent/src/answerers/entity.rs:1394-1407 | `with_supplemental` 用合并后行数 `>= 10` 判截断：客户/省区各 LIMIT 10，合并最多 20 行——6+6=12 也报截断（误报），10+10=20 反而对 | 按「任一路行数 ≥ 10」记 flag 透传，配测试 | test |
| crates/agent/src/answerers/entity.rs:1413 | `build_card` 的 `_title: &str` 是死参数，五个调用点各传一个无用字符串 | 删参数并同步五个调用点 | safe |
| crates/agent/src/answerers/entity.rs:1436,1445 | `contains("金额") |  | contains('额')`——「金额」必含「额」，前者是死条件（两处） |
| crates/agent/src/answerers/entity.rs:1620 | 测试函数头 `() {        for (question…` 同行塞了 for 且多余空格——rustfmt 漏网/未跑过的格式残渣 | 正常换行格式化 | safe |

## datamap.rs（29 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| datamap.rs:791-792 | 头注称「db 维度留在 evidence」，但四类边的 evidence JSON（516-527/538-546/554-560/701-708）均无 db 字段，注释与代码不符 | 改注释为「db 维度丢弃」或在 evidence 加 db 字段 | safe（改注释） |
| datamap.rs:835 | `split_ref` 文档同样声称「db 维度留在 evidence」，同上不符 | 同上，两处注释一起改 | safe |
| datamap.rs:99-105 | `null_rate()` 生产路径无人调用（仅 995 行测试用），空值率算完即弃 | 把 null_rate 加进 joinable/distribution 证据，或标注 `#[allow(dead_code)]` 说明预留 | safe |
| datamap.rs:361 | `dtype_bucket` 用 `contains("int")`，空间类型 `point` 会误判 numeric；`linestring` 含 "string" 误判 string 桶 | 先排除 `point/linestring/polygon/geometry` 等空间类型再归桶（lineage.rs:219 同步改） | test |
| datamap.rs:352 | 前缀比用 `chars().count()` 除以 `len()`（字节长），非 ASCII 列名相似度被系统性压低 | 分母改 `a.chars().count().max(b.chars().count())` | test |
| datamap.rs:389 | `trivial_domain` 每次调用对每个 key `to_ascii_lowercase()` 分配，且在 O(n²) 双层循环里每对调两次 | 画像构建时预算 `trivial: bool` 存入 ColumnProfile | safe |
| datamap.rs:495 | `sort_by(\ | a,b\ | a.id().cmp(&b.id()))` 每次比较重复分配两个 id 字符串 |
| datamap.rs:498 | `retain` 去重再次调 `p.id()` 分配；与排序合计每个画像分配 3+ 次 id | 排序后连续相等去重，或复用 cached key | safe |
| datamap.rs:505-508 | `and_modify` 里 `*old = edge.clone()` 克隆后 `or_insert(edge)` 又把原值丢弃，高一置信度命中时多一次整边克隆 | 改用 `match dedup.entry(..) { Occupied/Vacant }` 直接 move | safe |
| datamap.rs:523 | synonym 证据里重算 `name_similarity(&a.column, &b.column)`，`synonym_confidence` 内部刚算过 | 让 `synonym_confidence` 返回 `(conf, name_sim)` 或证据复用 | safe |
| datamap.rs:431/435/523 | O(n²) 循环内每对重复 `normalize_name`×2 + `name_similarity`（内部再 normalize×2）+ `dtype_bucket` 小写化 | 循环外为每个画像预存 normalized name 与 dtype 桶 | safe |
| datamap.rs:463-465 | `na <= 0.0 \ | \ | nb <= 0.0` 不可达：`counts` 非空 ⇔ `non_null > 0`，而 458-460 已挡 cardinality=0 |
| datamap.rs:582 | `paired_numbers` 里 `.trim()` 冗余：`cell_value`（232-235）已 trim 过 | 去掉 trim，直接 `parse` | safe |
| datamap.rs:688-695 | `col_id` 闭包逐字复制 `ColumnProfile::id()`（89-97）的全小写三段拼接逻辑 | 抽自由函数 `fqid(db, table, col)` 两处共用 | safe |
| datamap.rs:849+889 | `ensure_datamap_table` 在 `build` 和 `save_edges` 各跑一遍，每轮建图白付 3 句 DDL | `build` 里删掉 889 行那次（`save_edges` 自确保已够） | safe |
| datamap.rs:848-867 | 逐行 upsert 无事务：N 条边 N 次往返；失败留半截靠重跑收敛（文档 846 承认） | 包一层 `pg.begin()` 事务提交，失败整体回滚，重跑语义不变 | test |
| datamap.rs:850 | `save_edges` 是 pub 但对空 ds_id 无护栏，`ds_id=''` 会静默落库 | `anyhow::ensure!(!ds_id.is_empty(), ..)` | safe |
| datamap.rs:836-844 | `split_ref("c1")` → `("", "c1")`，畸形 src 会把 `left_table=''` 写进唯一键（DDL 无非空 CHECK 之外的长度约束） | `save_edges` 里跳过并 warn `lt.is_empty() \ | \ |
| datamap.rs:918 | `tables_skipped` 原因恒为静态串「采样被拒/失败/无可采样列」，真实原因只在 warn 日志里，报告丢失可追因性 | `profile_table` 改返回 `Result<Vec<_>, String>` 把具体原因带进报告 | safe |
| datamap.rs:914-918 | `Some(cols) if !cols.is_empty()` 的空 vec 分支不可达（cols 空 → `sample_sql` 返 None，282-285 已截住），match 臂冗余 | 简化为 `Some(cols) => … / None => skipped.push(..)` | safe |
| datamap.rs:892-895 | catalog `collect::<HashMap>()` 遇重名小写表静默后者覆盖前者；唯一性只靠目录测试约束，运行时无感知 | 用 `entry` 插入时 `debug_assert` 或 warn 重名 | safe |
| datamap.rs:912 | `cols_by_table.get(&key).cloned()` 每次克隆整个 `Vec<&ColumnInfo>` | 直接传 `&cols_by_table[&key]` 引用（签名改为 `&[&ColumnInfo]` 已兼容） | safe |
| datamap.rs:909 | `seen_tables.insert((asset.database, key.clone()))` 无论是否已存在都克隆 key | `if !seen_tables.contains(..) { insert }` 或 entry API | safe |
| datamap.rs:876-948 | `build` 全程无 info 级完成日志（lineage.rs:549 有），CLI 不打印报告就无任何落痕 | 末尾加 `tracing::info!(tables, edges, skipped, "建图完成")` | safe |
| datamap.rs:862 | `edge.confidence`（f64）bind 进 `real`（f32）列，隐式窄化丢精度，round4 的值入库后不再精确 | bind 前显式 `edge.confidence as f32`，窄化点显式化 | safe |
| datamap.rs:829-832 | 静态推断 upsert 不动 `last_seen`/`seen_count`，重跑后 last_seen 停在首次插入；与 usage 写口（datamap_usage.rs:84-85）口径不一 | 明确语义：若 last_seen 意为「最近被任何来源观测」，SET 里加 `last_seen = now()`；否则在头注写明仅 usage 维护 | test |
| datamap.rs:794-816 | 三处 DDL 逐字一致只靠 `tools/check_datamap_ddl.py` 外部闸，crate 内无测试钉 `DATAMAP_DDL == datamap_usage::DDL`（同 crate 可比） | 在 tests 里加 `assert_eq!(DATAMAP_DDL, crate::datamap_usage::DDL)`（需把后者 const 提为 pub(crate)） | safe |
| datamap.rs:715-755 | `sample_pair` 的闸门+fetch+回包列校验脚手架与 `profile_table`（286-307）高度重复 | 抽共用的「fetch 并校验回包列」helper，两处各留差异部分 | safe |
| datamap.rs:768-781 | `correlate_table` 28 对串行 await，每对一条 SQL；同一表本可一条 SELECT 全数值列 LIMIT 500 拿对齐样本，省 27 次往返且样本天然同行对齐 | 每表改一条联合采样 SQL 后在 Rust 侧逐对算相关 | test |

## KbDocPreview.vue（28 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| KbDocPreview.vue:18,29 | `FileKind`（L67 定义）、`TocEntry`（L211 定义）在使用之后声明，靠类型提升才能编译，阅读跳跃 | 类型声明移到文件顶部 | safe |
| KbDocPreview.vue:40-47 | `headers()` 与 KbAnswer.vue:129-136 `sessionHeaders()` 完全重复（连报错文案） | 抽公共 helper | safe |
| KbDocPreview.vue:56 | 错误兜底 `HTTP ${status}` 是英文技术串，混在全中文 UI 里 | 改「服务暂不可用（HTTP 500）」式文案 | safe |
| KbDocPreview.vue:59-62 | `extOf('.gitignore')` 返回 `'gitignore'`、`'a.tar.gz'` 返回 `'gz'`，角标与 `kindOf` 都会误判 | 点前无字符视为无扩展名；或维护已知双扩展名单 | safe |
| KbDocPreview.vue:70 | 图片扩展名缺 `avif`（mime 侧也缺 `image/avif`），avif 原件落「不支持预览」 | 补上 avif | safe |
| KbDocPreview.vue:77-81 | mime 兜底只认 image/pdf/text-plain：`text/csv`、`application/json`、`text/markdown` 的无扩展名文件全部落 `none` | mime 映射补齐 csv/json/markdown | safe |
| KbDocPreview.vue:107-109 | 分隔符嗅探按首行裸 `split` 计数，首行带引号包裹的分隔符会算错（如 `"a,b"`） | 嗅探时跳过引号内字符，或复用解析器跑一遍首行 | test |
| KbDocPreview.vue:134,140 vs 439 | 截断后实际保留「表头+200 数据行」共 201 行，提示语「仅预览前 200 行」有歧义 | 文案改「前 200 行数据」 | safe |
| KbDocPreview.vue:208 | 注释「md 原件直接复用 Markdown 页签的渲染器（同一份实现，不引入第二份）」——但 KbAnswer.vue 里还有第三份渲染器（render），仓库层面已是两份 | 注释收窄为「本文件内复用」，或推动渲染器合并 | safe |
| KbDocPreview.vue:223-260 vs KbAnswer.vue:195-262 | 两套手写 markdown 渲染器功能分叉：本文件不支持表格/引用块/hr/键行，md 原件里表格会渲染成竖线文本 | 中期合并为共享渲染器；短期至少在注释里互相指引 | safe |
| KbDocPreview.vue:242-244 + 441,463 | **id 撞车 bug**：`fileMarkdownHtml` 与 `markdownHtml` 都生成 `kdp-mdh-0…`，md 文件时两个 pane 经 v-show 同时在 DOM，`jumpToHeading` 的 `getElementById` 可能命中隐藏 pane 的同名 id，目录跳转失效 | `renderMarkdown` 加 id 前缀参数（如 `file-`/`tab-`） | test |
| KbDocPreview.vue:243 | TOC 文本取自 **esc 之后**的 `heading[2]`：标题含 `&<>` 时目录按钮显示字面 `&amp;` | TOC 文本在未转义原文上提取（仅去 `*`/反引号） | safe |
| KbDocPreview.vue:280 | `String(data.markdown ?? ...)` 若服务端误回对象/数字会得到 `[object Object]` 渲染出来 | `typeof === 'string'` 判断，否则按空处理 | safe |
| KbDocPreview.vue:306-308 | `heading_path` 数组用 `' / '` 连接展示，KbAnswer.vue:301 直接原样展示——同名字段两组件分隔风格不一 | 统一分隔符 | safe |
| KbDocPreview.vue:360-365 + 465-468,496-499 | `markdownRan/chunksRan` 失败也置 true，切走再切回永不重试，且三个页签的失败态都没有「重试」按钮（KbAnswer.vue:479 有） | 失败态补重试按钮（调对应 load 函数） | safe |
| KbDocPreview.vue:382 | 下载失败完全静默（catch 空）；用户站在 Markdown/Chunks 页签点「下载」失败时零反馈 | 加一个轻量错误提示（如头部按钮旁红字，自动消隐） | safe |
| KbDocPreview.vue:397 | 仅在 setup 调 `loadFile()`，无 docId watch：父级目前靠 `v-if` 重挂载（KbPanel.vue:2317）没问题，但一旦复用为切 doc 不关窗即内容滞留 | watch `() => props.docId` 重置状态重载，或父级加 `:key="previewDoc.doc_id"` | safe |
| KbDocPreview.vue:401-402 | Esc 关闭绑在 `<section>` 上，但 section 无 tabindex 不可聚焦：用户先点了遮罩再按 Esc 无效 | section 加 `tabindex="-1"` 并挂载时 focus，或监听 document keydown | safe |
| KbDocPreview.vue:402 | `role="dialog" aria-modal="true"` 但无焦点圈定：Tab 可移出到遮罩背后的页面 | 挂载聚焦 + 简单 focus trap（或至少 autofocus 首个可点元素） | safe |
| KbDocPreview.vue:404 | 扩展名角标 `.toUpperCase()` 未限长：`markdown`→「MARKDOWN」8 字符塞 34px/9px 盒子溢出 | `slice(0,4)` 或缩小字号自适应 | safe |
| KbDocPreview.vue:406-409 | `role="tablist/tab"` 但 tab 无 `aria-controls`、panel 无对应 `id`，也无方向键切换——半套 ARIA tab 模式反而误导 AT | 补全 id/aria-controls/键盘交互，或降级为普通 button 组 | safe |
| KbDocPreview.vue:434 | 模板里 `csvRows.slice(1)` 每次渲染新建数组；200 行×列重渲染时无谓分配 | 预计算 `csvBodyRows` computed | safe |
| KbDocPreview.vue:441-443 | 空 .md（`fileText=''`）与空 .csv（0 行）落入最后的「该格式不支持内嵌预览」——格式其实支持，是内容为空，文案错误 | 增加「文件为空」专属分支文案 | safe |
| KbDocPreview.vue:149-153,272-274,341-343 | 三段 fetch+401+errorText 流程结构雷同 | 抽 `fetchOrThrow(url)` 小helper | safe |
| KbDocPreview.vue:512 | 弹窗高 `min(760px, 100vh-44px)` 但遮罩 padding 22px（L508），`760px+44px` 在恰好 804px 视口时上下留白不等，视觉略偏；`calc(100vh - 44px)` 的 44 与 padding 22*2 是隐式耦合 | 用 `100%` 让 grid 居中自己算，或注释说明 44=2×22 | safe |
| KbDocPreview.vue:595-596 | TOC lv4/lv5/lv6 同为 26px 缩进，4-6 级标题视觉上无法区分 | 递增缩进（16/26/36） | safe |
| KbDocPreview.vue:545 | scoped 内裸 `button:disabled` 选择器影响组件内所有按钮（含未来子组件根节点），与 L622 KbAnswer 的 `.answer-feedback button:disabled` 局部写法不一致 | 限定到具体按钮类 | safe |
| KbDocPreview.vue:455 | TOC 阈值 `>= 2` 硬编码在模板，无注释说明「单标题不出目录」是刻意 | 加一行注释或提为常量 | safe |

## chat.rs（28 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| chat.rs:5-6 | 文件头用将来时描述 branch 接线（「路由行与 handler 由 main.rs 持有」），但 main.rs:1342 路由、main.rs:2140 handler 均已落地 | 改为事实陈述 | safe |
| chat.rs:11-12 | 头注释契约只写 `Ok` 两分支，漏了实存的 `Err(e) → 500 {"error": e.to_string()}`（main.rs:2151） | 契约补全 500 分支 | safe |
| chat.rs:142-144 | 🔴 注释「query_log 没有 conv_id」已过时：query_log.rs:55 已有 `conv_id` 列且 main.rs:1890 透传 | 理由改写为「失败行无 SQL、conv_id 事后贴回，per-conv 上一轮仍以 chat.msg 为准」 | safe |
| chat.rs:201 | 「接线后去掉本行」已到期（路由已接） | 删该注释行 | safe |
| chat.rs:108 vs 127-135 | doc 说「首条 user 消息设标题」，代码实为「任一 user 消息且标题仍是默认值即设」（首轮硬失败时第二问也会设） | 注释收紧为「首个仍处默认标题时的 user 消息」 | safe |
| chat.rs:53 | `migrate` 逐句 execute 不在事务里，中途失败留半迁移态（幂等可自愈但靠重启） | 包进一个 tx | test |
| chat.rs:33-52 | chat 的 DDL 无幂等单测（query_log.rs:451 `ddl_statements_are_idempotent` 有同款） | 补镜像测试 | test |
| chat.rs:53 | split(';') 纪律（注释内不许 ASCII 分号/`DO $$`）query_log.rs:74 有警告注释，本文件没有 | 补同款 doc 注释 | safe |
| chat.rs:62-63 | `ORDER BY updated_at DESC` 无并列键，updated_at 同值时 PG 不保证稳定序 | 加 `, id DESC` | test |
| chat.rs:63 | `LIMIT 100` 魔法数 | 提 `const MAX_LIST_CONVS` | safe |
| chat.rs:62 | `to_char(updated_at,'MM-DD HH24:MI')` 按 PG 会话时区渲染 timestamptz，PG TZ≠用户时区则侧栏时间整体偏移 | Rust 侧取 `updated_at` 再格式化，或 `AT TIME ZONE` | test |
| chat.rs:91 | `conv_msgs` 无 LIMIT，超长会话全量进内存并整包序列化 | 加防御性上限（如 1000）或注释写明刻意不设 | test |
| chat.rs:89-95 | `conv_msgs` 自身不做属主过滤，安全全靠每个调用点记得先 `conv_owner`（main.rs:2118 做了，新调用点忘一步即越权读） | doc 加 🔴「调用前必须过 conv_owner」纪律 | safe |
| chat.rs:116-135 | `save_msg` 的 INSERT/UPDATE updated_at/UPDATE title 三条顺序 SQL 无事务：INSERT 后崩溃 → 侧栏排序不刷、标题不设 | 包事务或合并为单条 CTE | test |
| chat.rs:116-135 | 每条消息 3 次（user 时 4 次）DB round trip | updated_at 刷新与 INSERT 合并成一条 CTE，省一次往返 | test |
| chat.rs:129 | 标题 `question.chars().take(18)` 未 trim、未剥控制字符，问句含 `\n`/前导空白直接进侧栏标题 | 先 `trim()` 并滤 `\r\n\t` 再 take(18) | test |
| chat.rs:130 | 标题 UPDATE 无空串守卫：question 为空会把标题刷成 `''` | 加 `AND $2 <> ''` 或 Rust 侧空串早退 | test |
| chat.rs:129 | 18 字截断无单测（query_log.rs:308 的 clip 有字符边界测试，这里没有） | 抽纯函数 `title_of` 并测多字节边界 | test |
| chat.rs:38/130 | 默认标题 `'新会话'` 字面量两处（DDL DEFAULT + UPDATE 谓词），改一忘一即标题永不刷新 | 提 `const DEFAULT_TITLE` 注入两处 SQL 文本 | safe |
| chat.rs:116/127 | `"user"`/`"ai"` 魔法串散落本文件与 8+ 调用点 | 提 `pub const ROLE_USER/ROLE_AI` | safe |
| chat.rs:222-225 | branch 前的 `count(*)` 是多余 round trip：PG `LIMIT n>total` 自然只复制现有行，唯一需要的是负数钳 0（LIMIT 负数报错）；count→copy 之间还有并发窗口 | 删 count 查询，`branch_cut` 退化为 `n.max(0)`/None→`i64::MAX` | test |
| chat.rs:238 | `copied as i64` 是 u64→i64 截断转换（理论超界回绕） | `i64::try_from(copied).unwrap_or(i64::MAX)` | safe |
| chat.rs:286-288 | 属主查询 `Err(_)` 吞掉真实 DB 错误零留痕（对比 main.rs:1858 last_turn 失败有 warn 纪律） | `warn!(conv_id, %e, ...)` 后再返 500 | safe |
| chat.rs:283-289 | 属主三态 match 在 api_ask(main.rs:1841)/api_conv_msgs(main.rs:2118)/steer 重复三遍，判据文案靠手工对齐 | 抽 `ensure_owner` helper（判据与文案一字不动） | safe |
| chat.rs:109 | `save_msg` 的 Result 被全部 6 处调用点 `let _ =` 静默吞（main.rs:1926-1927、deep_api.rs:3685/3687/4233/4235、xcx_api.rs:517-518、artifact_api.rs:475），与 query_log「失败只 warn」纪律不一致 | chat.rs 提供 `save_msg_logged` 封装（内部 warn）供调用点切换 | safe |
| chat.rs:174-181 | `delete_conv` 返 Ok 不代表删了行（0 行也 Ok），调用点 main.rs:2134 恒 `ok:true` | doc 注明「Ok ≠ 删了行，刻意不泄存在性」 | safe |
| chat.rs:270 | `headers.get("X-API-Key")` 用 &str 每请求做大小写不敏感匹配 | `HeaderName::from_static("x-api-key")` 常量复用 | safe |
| chat.rs:32-57 与 query_log.rs:75-80 | split(';') 逐句 execute 的 migrate 样板两文件各写一份（semantic 还有第三份） | 提共享 `run_ddl(pg, ddl)` helper | safe |

## cards.rs（28 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| cards.rs:44-50,54-59 | 语境词清单 `["订单","下单","售后","费用","活动","巡店","促销"]` 原样复制两份，漂移风险 | 提 `const FACT_CONTEXTS: &[&str]` 两处共用 | safe |
| cards.rs:53-60 | `ambiguous_sales_personnel` 无 fn doc：与 `personnel_dimension_allowed` 的分工（一个短路整路出澄清卡、一个过滤单条命中）只写在 L42 那半边 | 补 fn doc | safe |
| cards.rs:64-69 | 歧义短路返回澄清卡无日志：「维度卡为什么只剩一张澄清卡」排查无据 | `debug!` 一次 | safe |
| cards.rs:111 | `rows[matched[k].0].clone()` 5 元组整克隆，aliases 深克隆后丢弃（metric.rs:106 同族） | 按字段解构引用 | safe |
| cards.rs:122,152 | `load_terms` 同一问句跑两遍（recall_terms 波 1 + recall_term_mapped 波 3，gather.rs:74,170） | terms 作参数传入共享 | test |
| cards.rs:167-169 | 每命中一个术语 = 5 条 SQL（meta.metric 全扫 + meta.dimension 全扫 + 值域三条），与波 1 已加载行集完全重复；命中 3 个术语 = 15 条重复 SQL | 共享波 1 行集，或子召回收敛为 IN 查询 | test |
| cards.rs:167-169 | 三路子召回顺序 await，互不依赖 | `tokio::join!`（每术语省 2 个 RT） | safe |
| cards.rs:162 | `limit: 3` 魔法数无常量无注释（为什么 3） | 提常量 + 注释 | safe |
| cards.rs:175-179 | `card_name`：`【】`（空名）→ `Some("")`，dedup_new_cards 里所有空名卡互相误删 | `end == 0 → None`（漏判方向，与 L174 注释一致） | test |
| cards.rs:177 | 卡名含 `】` 时 `find` 截歪——卡名来自种子名，前提「卡名不含 】」未钉 | 注释钉前提 | safe |
| cards.rs:184-196 | `dedup_new_cards` 把 existing 全部卡名克隆进 `HashSet<String>`（每卡一次分配）；existing 侧可借用 | existing 用 `HashSet<&str>`，只 new 侧维护 owned seen | safe |
| cards.rs:203-209,297-299 | `recall_value_hints` 两条、`recall_value_domains` 三条加载 SQL 各自顺序 await，互不依赖 | `tokio::join!` | safe |
| cards.rs:202-210 vs 296-322 | 两函数同波并发（gather.rs:76-77）：`load_value_domains`、`load_value_maps` 同一 SQL 每问句并发打两遍 PG | 合并两路或共享加载 | test |
| cards.rs:223-228 | `is_domain` 闭包对每行线性扫 domains（O(rows×domains)，逐对 eq_ignore_ascii_case） | 预建小写键 `HashSet<(String,String)>` | safe |
| cards.rs:252,258 | `'{code}'`/`LIKE '%{code}%'` 直接插值：code/name 含 `%`/`_`/`'` 时模式注入或卡文案破（种子受控但无防线；L254-256 注释自述「LLM 照抄的是示例」） | 渲染前校验/转义，或种子卫生断言 | test |
| cards.rs:270-271 | `value_domain_card = value_domain_card_for("", …)`：同 metric.rs:133 的「""≠DMS」隐式契约无注释 | 补一行注释 | safe |
| cards.rs:300-321 | 每个 domain 都全扫 values 过滤 + `longest_value_hit` 内部再 sort | 预分组 by (table,column) 后逐 domain 查 | safe |
| cards.rs:303-305 | `same` 闭包签名 ` | t: &String, c: &String | ` —— `&str` 更顺（调用处随之简化） |
| cards.rs:332-333 | `cx.embed_slices.to_vec()` 全片 String 克隆（每片几百字符向量字面量）只为 bind | bind 引用（`&[String]` 可 Encode）或 `Vec<&str>` | safe |
| cards.rs:386-387 | `ORDER BY dist LIMIT $2` 无第二排序键：dist 并列时 LIMIT 边界行随物理序漂——与本仓自己修过的确定性账（metric.rs:78-81、schema.rs:79-84）同类 | 加 `, e.element_id` tie-break（改边界行） | test |
| cards.rs:380 | 相关子查询每行 unnest 整个切片数组；多向量 MIN 使 HNSW 索引用不上，1033 行顺序扫（量小但无注释说明这是有意的） | 注释钉「元素表量级下顺序扫可接受」 | safe |
| cards.rs:434 | `_ =>` 未知 kind 静默渲成「【术语·…】」卡：新 kind 拼错/新增时错配 invisible | warn 一次或显式列出 kind 集合 | safe |
| cards.rs:447 | 只有放宽路径有 info 留痕；严格档命中数无 debug——L446 注释自述「靠放宽救回来的频次是调参依据」，严格档那半数据缺失 | 严格档命中也 `debug!` 计数 | safe |
| cards.rs:422-444 | render 闭包对 rows 做两遍 filter+map（strict/loose 各一遍）；rows 已按 dist 升序，可在首个越界处截断（rows≤limit，仅记录） | take_while 或注释「量小两遍无妨」 | safe |
| cards.rs:429-435 | 元素卡前缀形态（`指标·/维度·/码值·/术语·`）被 gather 侧 `prompt_card_has_name` 的前缀清单依赖（gather.rs:709-713）做跨路去重——本文件无注释指明，改前缀会静默破去重 | 注释钉「前缀清单与 gather.rs:709 同步」 | safe |
| cards.rs:324-330 vs 394-397 | 「为什么不返 Result」的缘由写在函数中段（L394-397）而非 fn 签名 doc——读签名的人看不到 | fn doc 抄一句 | safe |
| cards.rs:420-421 | STRICT/LOOSE 注释说「与 `DS_MAX_DIST` 的实测距离表同源」但未给指针（哪个文件） | 注释补指针 | safe |
| cards.rs:537-550 | `dimension_hit_matching` 测的是 `#[allow(dead_code)]` 的 `dim_hit`（T7-3 保留品），生产判据是 L98-105 的 match_word+personnel_dimension_allowed——测试名/注释没写「非生产路径」，读者易当生产判据 | 测试注释点名 | safe |

## utils/useVoiceRecognition.js（27 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| utils/useVoiceRecognition.js:15-16 | 注释编号③④出现，但文件内无①②出处，新读者找不到图例 | 补①②或改述 | safe |
| utils/useVoiceRecognition.js:16 | 注释用直角引号「"停止/重播"」，与文件其它注释引号风格不一 | 统一引号 | safe |
| utils/useVoiceRecognition.js:49,149 | 文案「需要麦克风权限才能语音录入」两处硬编码 | 提常量 | safe |
| utils/useVoiceRecognition.js:252,381 | 文案「当前环境不支持语音识别，请手动输入」两处硬编码 | 提常量 | safe |
| utils/useVoiceRecognition.js:263,397 | 文案「语音识别仅在微信小程序内可用，请手动输入」两处（跨 #ifndef 块）硬编码 | 提常量到条件编译块外 | safe |
| utils/useVoiceRecognition.js:129,143 | 50ms 重启延迟幻数两处 | 提常量 `RESTART_DELAY_MS` | safe |
| utils/useVoiceRecognition.js:167 | 200ms 错误重试延迟幻数 | 提常量 | safe |
| utils/useVoiceRecognition.js:209 | `speak` 硬编码 `lang:'zh_CN'`，未用 options.lang；调用方传 `lang:'en_US'` 时识别英文但播报仍按中文合成 | 改用 options 的 lang（或独立 ttsLang） | test |
| utils/useVoiceRecognition.js:218 | onEnded 后不 `destroy()` InnerAudioContext，微信官方建议销毁；只有下次 stopSpeak 才清，长会话累积实例 | onEnded/onError 内 destroy 并置 null | test |
| utils/useVoiceRecognition.js:216 | 每次 speak 新建 InnerAudioContext，频繁播报有重复创建开销 | 复用单实例（切 src） | safe |
| utils/useVoiceRecognition.js:214,226 | 合成成功但无 filename、以及 fail 分支完全静默，无留痕 | 加 `console.warn('[tts]', ...)` | safe |
| utils/useVoiceRecognition.js:235 | `pressDownAt` 在权限 await 之前记录；授权弹窗耗时计入"按住时长"，用户授权后立即松手（实际录音 <500ms）不触发短按拦截，仅靠插件 -30011 兜底 | `pressDownAt` 移到 `mgr.start()` 成功后记录 | test |
| utils/useVoiceRecognition.js:279-282 + 234-267 | `shortPressPending` 依赖插件回调清除，L280 注释自认"插件对超短录音可能不回调"；一旦无回调标志残留，而 `start()` 不重置它 → 下一次正常录音的 onStop 在 L120 误吞 onFinal | `start()`/`startContinuous()` 开头重置 `shortPressPending = false` | test |
| utils/useVoiceRecognition.js:283 | `manager.stop()` 无 try/catch，与 hardReset L322-326 的防护写法不一致 | 统一包 try/catch | safe |
| utils/useVoiceRecognition.js:258,349,389 | `mgr.start()` 三处均无 try/catch，同步抛异常时 `started=true`/`__seq` 已置，状态滞留 | 包 try/catch，失败回滚 started/seq | test |
| utils/useVoiceRecognition.js:37-42 | 调用方未传 `onTooShortCb` 时短按完全无用户反馈（无默认 toast） | 兜底 `uni.showToast({title:'说话时间太短',icon:'none'})` | test |
| utils/useVoiceRecognition.js:295 | 看门狗 65000 仅比 duration 60000 多 5s；插件 60s 自动停的 onStop 若延迟 >5s 会误杀健康会话 | 余量加宽（如 75s）或注释论证 5s 足够 | test |
| utils/useVoiceRecognition.js:309 | 文案「请重新点话筒」与文件其它处「语音录入/手动输入」用语不一 | 统一措辞 | safe |
| utils/useVoiceRecognition.js:149-156 | permission 分支先设 `errorText` 再弹 `showAuthGuide`，页面若同时 toast errorText 会与 modal 双重提示 | permission 分支不设 errorText 或注释约定页面不提示 | test |
| utils/useVoiceRecognition.js:236 vs 385-386 | `start()` 在权限检查前清 recognizedText/errorText，`startContinuous()` 在权限检查后清，同款逻辑两处顺序不一 | 统一清理时机 | safe |
| utils/useVoiceRecognition.js:196 | `isSpeaking.value = false` 在 #endif 之外，非微信端也写状态，与 L16 注释「仅反映播报」口径在非 MP 端无意义 | 移入 #ifdef 或注释说明 | safe |
| utils/useVoiceRecognition.js:46-56 | `showAuthGuide` 无防重入锁，权限拒绝场景连续触发会叠多个 modal（ai-chat/index.js:23 已有 `isShowingLoginModal` 先例） | 加 isShowing 锁 | safe |
| utils/useVoiceRecognition.js:90 | 生产环境 `console.error` 输出完整 err 对象，量大且可能含敏感路径 | 降级为 warn 或按 env  gate | safe |
| utils/useVoiceRecognition.js:8 | 每次调用创建独立闭包，但底层 WechatSI manager 是单例，两个组件同时使用会互相覆盖 onStart/onStop/onError | 文件头注释警告"单例使用"，或模块级缓存 | safe |
| utils/useVoiceRecognition.js:9 | `duration` 未校验范围（微信单段上限 60000），传更大值插件行为未定义 | `Math.min(duration, 60000)` 并注释 | safe |
| utils/useVoiceRecognition.js:305-311 | 看门狗触发 hardReset 时 L336 清空 recognizedText，用户已说内容直接丢失且无透出 | onErrorCb 前把残留文本作为参数透出 | test |
| utils/useVoiceRecognition.js:124 | `res?.result |  | recognizedText.value |

## tools/settings.py（27 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| tools/settings.py:36-37 | 注释「+ 两个特例」指 mysql_targets/mcp_keys，但读者要自己对号 | 直接点名这两个字段 | safe |
| tools/settings.py:52-66 | Rust 侧（db.rs:478/481）对短钥匙/机器指纹兜底都 warn，Python 侧静默派钥，工具链零提示 | 首次派生机器指纹钥时 stderr 提醒一句，或注释说明刻意静默 | safe |
| tools/settings.py:58 | 与 Rust crypto.rs:133 一致地「不 trim 环境变量」，但无任何注释防后人好心加 `.strip()` 造成两侧钥分叉 | 注释「与 Rust 对齐：不 trim，文件挂载 secret 的尾换行两侧同语义」 | safe |
| tools/settings.py:61-62 | host/user 双缺时两台裸容器派同一把固定钥，注释只说跨机不可迁移 | 注释补一句「双缺 = 跨机同钥」 | safe |
| tools/settings.py:84 | `12 + 16` 魔数 | 提 `_NONCE_LEN=12`、`_TAG_LEN=16` 常量 | safe |
| tools/settings.py:87 | `.decode("utf-8")` 失败被吞进「钥匙不对/指纹已变」文案，非 UTF-8 明文会误判排查方向 | 单独 `except UnicodeDecodeError` 给「明文非 UTF-8」 | safe |
| tools/settings.py:106 | `list(targets.items())` 没有删键操作，list 拷贝多余 | 直接 `targets.items()` | safe |
| tools/settings.py:114 | mcp_keys 两条密文解出同一明文键时后者静默覆盖前者 | 撞键时 SystemExit 告警 | safe |
| tools/settings.py:125 | `DMSAI_SETTINGS=""` 空串 → `ROOT/"."` 是目录 → `read_text` 抛 IsADirectoryError 裸栈 | 空白按默认值处理或明确报错 | safe |
| tools/settings.py:137 | `json.loads` 失败抛 JSONDecodeError，消息内嵌文件内容片段——settings.json 正是凭据所在，错误回显即泄漏面 | 包 SystemExit「{path} 不是合法 JSON」不带内容（对齐 Rust db.rs:485） | safe |
| tools/settings.py:153 | 端口缺省 else 5432：`doris://` 等 mysql 系以外的 scheme 无端口时静默按 5432 | 未知 scheme 明确报错或注释限定本函数只服务 mysql/postgres | test |
| tools/settings.py:156 | dbname 为空串不校验，驱动对空库名行为不直观 | 空库名 SystemExit | safe |
| tools/settings.py:170 | 键存在但值为空串时报「里没有 pg_url」，与实情不符 | 区分「缺键」/「值为空」两套文案 | safe |
| tools/settings.py:206 | mysql_url 缺失时 `_dsn("", "mysql_url")` 报「不是可解析的 URL」而非「缺 mysql_url」 | 先判缺失再解析 | safe |
| tools/settings.py:222 | endpoint 判同只比 host 字符串，主机别名/IP 指向同机判不出 | 注释说明该局限 | safe |
| tools/settings.py:241-242 | magic 名 `"doris_warehouse"` 出现两次且无出处说明 | 提常量并注释为何优先它 | safe |
| tools/settings.py:254,265,276 | `scheme.startswith("mysql")` 让 `mysqlfoo://` 也过关 | 改精确匹配或注释说明取舍 | test |
| tools/settings.py:254+256 | `urlsplit` 解析一次后 `dsn()` 内再解析同一 URL | 让 `_dsn` 顺带返回 scheme 或合并校验，只解析一次 | safe |
| tools/settings.py:264-267 | `analysis_target` 内部已 `_dsn` 解析过，返回后 267 行第三次解析同一 URL | 让 analysis_target 返回解析好的 kwargs | safe |
| tools/settings.py:251-259 vs 262-270 | 两函数收尾三行（pop dbname→database、加 charset）逐字重复 | 抽公共小助手 | safe |
| tools/settings.py:130-137 | `load()` 无缓存，未透传 cfg 的链路每次重复读盘+解密 | 注释「调用方请透传 cfg」或 lru_cache | safe |
| tools/settings.py:177 | 三元里 `raw.strip()` 算两次 | 先 `s = raw.strip()` 再判断 | safe |
| tools/settings.py:282 | 自检首条只覆盖相对路径分支 | 补 `DMSAI_SETTINGS=/abs/path` 分支断言 | safe |
| tools/settings.py:425 | 口令判据大小写敏感，`PASSWORD='x'` 抓不到 | 加 `re.IGNORECASE` | test |
| tools/settings.py:425 | 只盯 `password`，`passwd=`/`pwd=` 字面量赋值漏 | 扩词，或注释说明刻意收窄 | test |
| tools/settings.py:430-436 | 只扫 `tools/*.py` 顶层，子目录（如 tools/kb_fixtures 日后放 .py）漏扫 | `rglob("*.py")` 并排除 `__pycache__` | test |
| tools/settings.py:434 | `p.read_text(encoding="utf-8")` 遇非 UTF-8 的 .py 自检直接崩 | try/except UnicodeDecodeError 跳过或 errors="replace" | safe |

## doc_graph.rs（27 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| doc_graph.rs:103-105 | `esc` 先剥光所有反斜杠：实体名里的 `\n`/`\t` 静默变 `n`/`t`（数据损毁而非转义）；两次 replace=两次全串分配 | 单趟 chars 实现 + 注释说明「剥除是防 AGE 转义歧义的刻意取舍」（与 graph.rs 同改） | test |
| doc_graph.rs:199-201 + 103-105 | 🔴 `cypher_sql` 用 `$$ ... $$` 美元引号，而 `esc` 不处理 `$`：LLM 抽取的实体名/关系文本（1076 行注释自认不可信）含 `$$` 即提前终结 dollar-quoting → SQL 语法错甚至注入 | esc 增 `$$` 剥离，或 cypher_sql 换唯一 tag（如 `$kb$`）并加测试 | test |
| doc_graph.rs:108-110 | `unquote` 的 `trim_matches('"')` 剥掉首尾所有 `"` 且不反转义 `\"`：实体名含引号时读回与写入不一致；`trim()` 还吃掉名字首尾空白（写侧 esc 不 trim，读写口径不对称） | 只剥一层成对外引号 + `\\"`→`"` 反转义 | test |
| doc_graph.rs:113-120 | `age_conn` 每次获取连接跑两条独立语句（LOAD + SET）= 两次 round trip，与 graph.rs:155-160 逐字重复 | `sqlx::raw_sql("LOAD 'age'; SET search_path = ag_catalog, public")` 一次往返（两侧同改） | test |
| doc_graph.rs:124-138 | `labels_ready` 每次全量查 catalog：`stats()` 内最多调 3 次（400/420/437）、`subgraph` 2 次（363/377）、`neighborhood` 2 次（775/813） | 一次查回该图全部 label 名进 HashSet，Rust 侧判断 | test |
| doc_graph.rs:149 | 纯静态串用 `format!("SELECT create_graph('{GRAPH}')")` | 直接字面量或 `concat!` | safe |
| doc_graph.rs:175-181 | 🔴 `write_chunk` 四条语句顺序执行但无事务：中途失败留下「有 Chunk 节点、无 MENTIONS/RELATION」的半成品，违反 1003-1004 行依赖的「关系端点必登 MENTIONS」不变量，残留只能等下次重建 | `conn.begin()` 包事务，四条全成才提交 | test |
| doc_graph.rs:158-171 | `clear_space` 两标签两次独立 DETACH 无事务：Chunk 删完 Entity 失败 → 窗口期剩孤实体（重跑幂等可收敛，但窗口内 dangling 口径失真） | 同事务包裹 | test |
| doc_graph.rs:296 | `nodes_cypher` 的 `ORDER BY count(*) DESC` 无 tie-break，而 488/586 行同类查询都有 `, e.id`/`a.id, b.id`——等重时 TOP-limit 结果不稳定，下游 edges_cypher 的 node_ids 随之漂移 | 补 `, e.id` | test |
| doc_graph.rs:315 | `edges_cypher` 同样缺 tie-break（对比 586 行有） | 补 `, a.id, b.id` | test |
| doc_graph.rs:724 | `neighborhood_edges_cypher` 同样缺 tie-break | 补 `, a.id, b.id` | test |
| doc_graph.rs:343 | `fetch_text_rows` 里 `cols.split(',').count()` 在**每一行**的闭包里重算（334 行构造 SELECT 时已算过一次） | 循环外 hoist `let ncol = cols.split(',').count()` | safe |
| doc_graph.rs:344 | `try_get::<Option<String>,_>(i).ok().flatten().unwrap_or_default()`：列解码失败静默变空串——AGE 升级改 ::text 形态会变成「空名字实体」而非报错 | 失败时 `tracing::warn!` 一次或 propagate | test |
| doc_graph.rs:373,388,620,662,710,816,884,952 | 全家桶 `.trim().parse().unwrap_or(0)`：解析失败静默成 0；其中 662/710 行 chunk_id 失败会**伪造 chunk_id=0 的记录**混进召回/PPR 原料 | parse 失败 filter 掉该行 + warn（或 Err），至少 chunk_id 两处不许 default | test |
| doc_graph.rs:543 | 同一 WHERE 里 `position(lower(nm) in lower($1))` 双小写，而 `word_similarity(nm, $1)` 两侧都不 lower——大小写口径不一致 | 统一 lower 或注释说明刻意 | test |
| doc_graph.rs:557 vs 570 | 空检用 `query.trim().is_empty()`，但 570 行 bind 的是未 trim 原串，首尾空白带进 similarity/position | bind `query.trim()` | test |
| doc_graph.rs:24-25 | 头注「`entities_named_like`……也是本文件第一个带 bind 的查询」与代码不符：`labels_ready`（128-136）早就 `.bind(GRAPH).bind(labels)` | 改为「第一个把**用户输入**走 bind 的查询」 | safe |
| doc_graph.rs:5-6 vs 1048-1055 | 头注称 esc/unquote 与 graph.rs 的一致「靠两侧同文 + 源码判据守住」，但 1049-1055 的测试只断言固定期望值，不做跨文件同文比对——改了 graph.rs::esc 及其测试后本侧测试仍绿，漂移无人发现 | 测试里 `include_str!("graph.rs")` 断言两侧函数体同文 | safe |
| doc_graph.rs:282-288 | `doc_list` 名不符实：589/728 行装 frontier/centers、741/756 装实体 id、1018 行装 entity_ids——它是通用字符串清单内联器 | 改名 `str_list`（私有函数纯改名） | safe |
| doc_graph.rs:800-802 | 死分支：771 行已短路 centers 空，795 行 chain 首段即 centers，centers 非空 ⇒ `ids` 必非空 | 删除或改 `debug_assert!(!ids.is_empty())` | safe |
| doc_graph.rs:817 | 权重回填 `nodes.iter_mut().find(\ | n\ | n.id == id)` 是 O(nodes)/行：nodes 可达 ~2×limit+centers（数千），mention_weights 行数同量级 ⇒ O(n²) |
| doc_graph.rs:358 vs 513,567,612,657,705,780 | clamp 风格不统一：`subgraph` 在入口 clamp，其余全塞在调用点实参里 | 统一为入口 clamp（纯代码移动，clamp 幂等） | safe |
| doc_graph.rs:870-886 | `chunk_nodes` 被 `GRAPH_SCAN_ROWS` 截断时本层无 truncated 标记；858 行注释承诺「截断看得见」实际依赖调用方自行比较 `len()==100_000`，契约隐式 | 返回 `(rows, truncated)` 或注释指明调用方判据 | test |
| doc_graph.rs:891-902 | `dangling_entities_cypher` 过滤 `e.space_id` 但 MENTIONS 的 c 不带 space 谓词：跨空间提及实际不可能全靠「实体 id 含空间散列」（9 行）的写侧纪律，无注释点明 | 一行注释钉住此前提 | safe |
| doc_graph.rs:206 | Chunk 的 MERGE 键是 `(doc_id, chunk_id)` 不含 space_id，space 靠 SET 后补：安全性依赖「doc_id 全局唯一」的调用方契约，无注释 | 一行注释钉住前提 | safe |
| doc_graph.rs:42 | 注释「HTTP 侧另有 500 的钳制」靠人肉维系双钳制关系，HTTP 侧改大时注释静默过期 | 注释互指 HTTP 侧常量名/位置 | safe |
| doc_graph.rs:1242 | 源码锚测试 `body.split("\n}\n")` 对函数结尾排版极敏感（函数后多一空行即切片错位、假红/假绿） | 改用更稳的锚（如 split 到下一个 `pub async fn`） | safe |

## tools/secret_scan.py（26 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| tools/secret_scan.py:16,130 | `".env"` 在 CONFIG_DOC_EXTS 里但 `Path(".env").suffix == ""`，名为 `.env` 的文件永远进不了私网 IP/企业 ID/用户路径三条规则——真实 bug | 判定改看 `path.name` 特例或 `(path.suffix or "."+path.name).lower()` | test |
| tools/secret_scan.py:16,130 | 同上原因，`.env.example`/`.env.local` 的 suffix 是 `.example`/`.local`，同样漏这三条规则 | 一并按文件名前缀 `.env` 特判 | test |
| tools/secret_scan.py:17-22 | BINARY_EXTS 缺 `.ico/.jar/.war/.exe/.dll/.so/.bin/.dat/.mp4/.woff/.woff2/.ttf/.lock` 等常见二进制/产物 | 增补 | test |
| tools/secret_scan.py:32-35 | LOCAL_PATH_PARTS 有 `logs` 无 `log`、有 `temp` 无 `tmpfs`，单数目录漏 | 补 `log` 等，或注释说明取舍 | test |
| tools/secret_scan.py:40-43 | FAKE_PASSWORDS 缺 `12345678/qwerty/admin123/passw0rd` 等高频弱口令，URI 里 8 位弱口令会被当真凭据报 | 增补高频弱口令 | test |
| tools/secret_scan.py:50 | private-key 正则漏 `-----BEGIN PGP PRIVATE KEY BLOCK-----`（"PRIVATE KEY" 后要求紧跟 `-----`） | 扩展可选尾部 | test |
| tools/secret_scan.py:52-54 | URI_RE 不含 `https?://user:pass@host`、`clickhouse://`、`mssql://`、`amqp://` 等可带凭据的 scheme | 至少补 http(s) userinfo | test |
| tools/secret_scan.py:57-59 | NAMED_SECRET_RE 只认引号包裹的值，未加引号的 `password = abcdef123456` 漏报 | 注释说明取舍，或补一条无引号规则 | safe |
| tools/secret_scan.py:60-63 | PRIVATE_IP_RE 不校验 0-255，`10.999.1.1` 会误报 | 注释「宁宽勿漏」的取舍 | safe |
| tools/secret_scan.py:65 | USER_HOME_RE 只认 `[A-Z]:\Users\`，macOS/Linux 的 `/Users/x`、`/home/x` 漏 | 扩展两个 POSIX 形态 | test |
| tools/secret_scan.py:69-72 | 扫的是工作区内容，index 与工作区不一致（staged 版本含密、工作区已改）时漏扫 | 注释说明，或改扫 staged blob | safe |
| tools/secret_scan.py:69-71 | `git ls-files` 无 timeout，异常仓库/巨型仓挂死无反馈 | `timeout=120` | safe |
| tools/secret_scan.py:72 | `out.decode("utf-8")` 遇非 UTF-8 文件名直接崩 | errors="surrogateescape" 或 "replace" | safe |
| tools/secret_scan.py:90-91,122-133 | `line_number` 每次从头 O(n) 数换行，多命中的大文件 O(n·m) | 预计算换行偏移列表 + bisect | safe |
| tools/secret_scan.py:96 | `uri.rstrip(".,);]")` 未含 `}`，JSON/模板里 `"mysql://…@h/db"}` 结尾的 `}` 会进 host 解析 | 补 `}` | test |
| tools/secret_scan.py:104 | `ipaddress.ip_address(host)` 在同一表达式里解析两次 | 提局部变量 | safe |
| tools/secret_scan.py:101,107 | 阈值 `>=8`、私网 `>=6` 的启发式无任何注释 | 加一行注释说明 | safe |
| tools/secret_scan.py:116 | `len(set(compact.lower())) >= 8` 熵启发（防 `aaaa…` 长串误报）无注释 | 加注释 | safe |
| tools/secret_scan.py:137-162 | scan 全程无进度输出，大仓首次跑长时间静默 | 可选 verbose 或每 N 个文件打点 | safe |
| tools/secret_scan.py:152 | `read_bytes` 与 ls-files 之间文件被删 → FileNotFoundError 裸崩（竞态） | try/except OSError 跳过 | safe |
| tools/secret_scan.py:158-160 | GBK 等非 UTF-8 文本（Windows 常见）整条当 finding 报 `non-utf8-file`，可能噪音 | 注释说明刻意严格 | safe |
| tools/secret_scan.py:165-174 | self_check 未覆盖 named_secret_finding、PRIVATE_IP_RE、`.env` 分支（上面那条 .env bug 若有此用例早暴露） | 补这三类断言 | safe |
| tools/secret_scan.py:183 | `finding(s)` 文案粗糙 | 按数量单复数输出 | safe |
| tools/secret_scan.py:185 | 行号 0 表示整文件 finding 是隐含约定，无注释 | 加一行注释 | safe |
| tools/secret_scan.py:186 | 命中行只有规则名，无任何修复指引 | 每条附一句 hint（如「凭据只许住 settings.json」）或末尾打印规则图例 | safe |
| tools/secret_scan.py:2 | docstring「Git-add candidate」偏窄：实际扫 tracked + 未忽略的 untracked | 文案改准 | safe |

## postgres.rs（25 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| postgres.rs:44 | `Mutex<DsPolicy>` 毒化即 panic（223/235 `.expect("pg 策略锁中毒")`），而同仓 registry.rs:162、source.rs:151 均用 `into_inner()` 容错；`DsPolicy` 是 Copy、持锁段无 panic 点，容错零成本 | 统一为 `unwrap_or_else(\ | e\ |
| postgres.rs:62 | `max_connections(max_conn)` 无 `.max(1)`（mysql.rs:448 先例），`max_conn=0` 配置在 PG 侧原样进池 | 与 MySQL 侧对齐钳制 | test |
| postgres.rs:62-78 | 只读源池未设 `application_name`（可带 ds_id），运维在 `pg_stat_activity` 无法归因连接属于哪个源 | connect options 设 `application_name` | safe |
| postgres.rs:79-94 | 表白名单只在 connect 采一次；registry.rs:73-84 按 ds_id 缓存源、`close` 仅 ds_api.rs:171 调用 → 同一 ds 二次上传新表后白名单过期，`enforce_schema` 恒拒新表直到重启——潜在 bug | 上传重建源时 `close` 旧池，或白名单改懒刷新 | test |
| postgres.rs:81-88 | 白名单查询无 timeout；公网/慢链路下 connect 悬挂（mysql.rs:38-39 为同类探针给 60s 预算的先例） | `tokio::time::timeout` 包裹 | test |
| postgres.rs:83 | 白名单取 `relkind IN ('r','p')` 含分区表，但 `table_probe`（dialect.rs:73-77）只取 `'r'`：分区表能过闸却采不进 schema，两侧口径不一 | 两侧统一 `relkind` 集合 | test |
| postgres.rs:103 | F3 失败时池靠 `Drop` 隐式关闭；mysql.rs:426 有 `pool.close().await` 先例 | 返回 Err 前显式 `src.pool.close().await` | safe |
| postgres.rs:113-118 | `owned_schema_visible` 无 timeout；`/api/health` 走它，PG 挂起则健康检查悬挂 | 包一层短 timeout（超时按可见=危险或单报，按 health 语义定） | test |
| postgres.rs:146-157 | `function_names_of` 与 `table_refs_of` 各自 `walk`（全量 parse）一遍（ast.rs:144-151），上传源每次 fetch/explain 重复 parse 两次，且 kernel check 已 parse 过一次 | kernel 加「一次 walk 出两者」的 API，此处调一次 | test |
| postgres.rs:164 | `expect("AST 实表名非空")` 把不 panic 押在另一个 crate 的 `retain(!parts.is_empty())`（ast.rs:181）上 | `let Some(t) = parts.last() else { continue };` 同成本消 panic 路径 | safe |
| postgres.rs:175-196 | 函数黑名单缺 `pg_create_restore_point`/`pg_switch_wal`/`pg_backup_start`/`pg_promote` 等管理函数（需特权，属纵深防御缺口） | 补进 `matches!` 清单并加对应用例 | test |
| postgres.rs:237 | `fetch_all` 把全部行物化进内存再由 `to_table` 截断到 max：无 LIMIT 大查询内存峰值=全量结果 | 改 `fetch()` 流 + `take(max)` | test |
| postgres.rs:295-309 | `to_table` 先 push 后判 `>= max`：`max=0`（DsPolicy 合法最紧档，source.rs:71 承诺「恒空结果」）实际返回 1 行，与契约矛盾——潜在 bug；mysql.rs:1142 同形态 | 改 `rows.iter().take(max)` 或先判后 push，补 max=0 用例 | test |
| postgres.rs:295-309 | 每行 `if i == 0` 分支、每 cell 重算 `type_info().name()` + `pg_cell_kind` | 循环前 `rows.first()` 提列名、预算一次 `Vec<Cell>` 逐列复用 | safe |
| postgres.rs:293-294 | `columns`/`data` 无容量预分配 | `data` 按 `rows.len().min(max)` `with_capacity` | safe |
| postgres.rs:259-263 | `explain` 用 `fetch_all` 物化计划行再整体丢弃（`Ok(Ok(_)) => Ok(None)`) | 改 `.execute()`，不解码计划行 | safe |
| postgres.rs:247-266 | `explain` 不做 ds_policy clamp（fetch 在 235 行做了）：ds 级收紧对 explain 不生效，两入口口径不一 | 入口同样 `clamp` 后再 `timeout` | test |
| postgres.rs:268-289 | `probe_schema` 两条探针串行且无 timeout | `tokio::join!` 并行 + 统一超时 | test |
| postgres.rs:317 | `SMALLSERIAL/SERIAL/BIGSERIAL` 是死分支：sqlx `type_info().name()` 对 serial 列返回底层 `INT4/INT8`（serial 是建表语法不是类型） | 删去或注释说明保留理由 | safe |
| postgres.rs:321+346 | `Cell::Text` 只 `try_get::<String>`：BOOL/UUID/JSONB 列 sqlx 拒解 String → 静默全 `Null`；测试 440 行还把 BOOL/UUID 钉在 Text 档——布尔列查出全 null 属潜在 bug | Text 臂加 `bool`/`Uuid` 等回落并更新映射测试 | test |
| postgres.rs:325-327,343 | 时间格式 `%Y-%m-%d %H:%M:%S` 丢毫秒；TIMESTAMPTZ 经 `naive_utc()` 按 UTC 渲染却不标时区（与 MySQL 侧一致的既定形态，但口径无文字记录） | `fmt_dt` 注释写明「秒级、UTC」口径 | safe |
| postgres.rs:107-109 | `fixed()` 无 doc 注释，读库上的静态写入面值得一句说明 | 补「静态 SQL 通道，仍受会话只读约束」 | safe |
| postgres.rs:40-42 | `schema`/`tables` 两个 `Option` 必须同有同无（145 行靠运行期 config 错误兜底），非法态类型上可表示 | 合并为 `Option<(String, HashSet<String>)>` | safe |
| postgres.rs:253-258 | `explain` 内联 `self.ds.as_str()` 而 `fetch` 绑定局部 `at`，同文件两种风格 | 统一绑定局部 `at` | safe |
| postgres.rs:449-455 | `include_str!("postgres.rs")` 自指字符串测试对重构脆（改名/拆文件即 panic）；目前靠 `expect` 响亮失败，可接受但未写明是故意脆 | 注释一句「故意脆：接线守卫」 | safe |

## kg.rs（24 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| kg.rs:121 | `normalize_name` 先 `collect::<Vec<_>>()` 再 `join`，中间 Vec 纯浪费 | 改 `fold`/手写循环直接拼单个 String，省一次分配 | safe |
| kg.rs:134 | `entity_id` 用 `format!` 造临时 String 仅为取 hash；`str::hash` 等价于「写字节 + 0xFF」，可手写 `h.write(a); h.write(b":"); h.write(b); h.write_u8(0xff)` 零分配且 hash 值逐位不变 | 换实现并加回归测试钉死现有 id 值（如 `entity_id("sp1","差旅报销")` 的期望字面量） | test |
| kg.rs:157 | 噪声第三族只认列出的日期/金额字符，纯标点名（如「——」「…」）仍漏进图 | 加一条「至少含一个字母/数字/汉字才放行」的兜底判定 | test |
| kg.rs:250-253 | 「抽取响应里没有完整 JSON 对象」文案重复两份，改一处忘另一处 | 提一个 `const` 或合并成单个 `ok_or` 分支 | safe |
| kg.rs:257 | 首次解析失败就无条件 `drop_trailing_commas(candidate)` 全量分配，多数失败响应根本没有尾逗号 | 先用 `memchr` 式快查（含 `,}`/`,]` 或逗号后仅空白收尾）再决定是否走修复分支 | safe |
| kg.rs:266 | 单行围栏（```` ```json{"a":1}` ``` ````，无换行）被 `split_once('\n')` 打成空串，明明有 JSON 却报「没有 JSON 对象」 | 无 `\n` 时退化处理：剥 `json` 前缀再试，而不是直接给空串 | test |
| kg.rs:278 | `drop_trailing_commas` 先 `chars().collect::<Vec<char>>()`，内存翻倍 | 改 `peekable` 迭代单遍扫，省 Vec | safe |
| kg.rs:321 | `truncate_chars` 恒分配 String，调用点（如 L453 拼 prompt）其实只需要切片 | 增一个 `&str` 版本（`char_indices().nth(max)` 取切点），分配版留给 push_sample 等真正需要的地方 | safe |
| kg.rs:33 | 注释称 MAX_ITEMS_PER_CHUNK「防撑爆图写批」，但 L371 关系端点自动补登可再塞进最多 2×50 个实体，图写批真实上界是 150 实体+50 关系 | 注释写清真实上界；若要在图投影层也封顶，`to_chunk_graph` 加总量闸 | safe（改注释）/test（加闸） |
| kg.rs:355 | 关系 label 兜底 `r["text"]` 是 prompt schema 外的宽容路径，无注释说明来源 | 补一行注释（兼容某些模型把关系名放 `text` 字段的实测形态） | safe |
| kg.rs:379 | `seen_rel` HashSet 未预留容量，按契约上限也就 50 条 | `HashSet::with_capacity(MAX_ITEMS_PER_CHUNK)`，顺手给 `relations` 也 reserve | safe |
| kg.rs:433 | `entry(label.clone())` 无论 key 是否存在都先克隆一次 | 先 `get_mut(&label)` 命中则不克隆，miss 才 `insert(label, …)` | safe |
| kg.rs:437 | 多数决每次都 `ballots[idx].get(&entities[idx].label)` 反查当前 label 票数 | 在 entity 旁存 `cur_votes: usize`（换 label 时同步更新），省一次哈希查 | safe |
| kg.rs:452/L479 | `extract_once` 每次都 `truncate_chars`，同一 chunk 重试 3 次就截断 3 遍同一文本 | 在 `extract_with_retry` 入口截一次，循环内复用 | safe |
| kg.rs:467 | `extract_with_retry<L: ChatModel>` 要求 `Sized`，而 `extract_once` 已是 `?Sized`，口径不一 | 统一加 `?Sized` | safe |
| kg.rs:479-482 | 解析失败（模型输出形状垃圾，重试大概率同样垃圾）与传输失败同等重试，白烧最多 2 次 LLM 调用 | 区分错误类：传输/空内容重试，纯解析失败可少试或注释说明「温度 0.1 下重试仍有收益」的取舍 | test |
| kg.rs:518 | `gate.acquire_owned().await` 的 `Result` 被静默丢掉：信号量若被关闭（当前不会）permit 为 None，任务照样跑且无限流 | `.expect("build gate 从不关闭")`，把假设写成显式断言 | safe |
| kg.rs:512-528 | 2000 个任务各自持有完整 `chunk.text`（库里多长就多长）一次性常驻，截断发生在任务内部 | spawn 前统一截到 MAX_CHUNK_CHARS，峰值内存立降且语义不变 | safe |
| kg.rs:542 | 每完成一个 chunk 就 `progress.report` 落一次库，2000 chunk = 2000 次 `meta.kb_graph_build` 写 | 计数/时间节流（如每 25 条或 500ms 报一次，终态必报） | test |
| kg.rs:539 | JoinError 分支用 `doc_id=""`、`chunk_id=-1` 魔法值，无注释，status 端点消费者看到会懵 | 加注释说明哨兵含义（FailedSample 是 wire 形状，不改类型） | safe |
| kg.rs:549 | 错误样本截断长度 300 是散落的字面量，与文件顶部的常量族风格不一致 | 提 `const MAX_SAMPLE_ERR_CHARS: usize = 300;` | safe |
| kg.rs:896 | 测试里 `src.split("fn build_chunks_sql").nth(1).unwrap()` panic 信息不带上下文（上方同类用法有 `panic!("{f} 不见了")`） | 统一成带锚点名的 panic 文案 | safe |
| kg.rs:504-505 | `map_err( | e | KbError::Db(e.to_string()))` 连续重复两次 |
| kg.rs:23-24 | 注释说「共 RETRY_MAX+1 次尝试」，与 L474 `0..=RETRY_MAX` 一致但测试 L873 才钉住；常量本身无跨文件引用保护 | 无需改代码；可选在常量注释里指向 `retry_is_exponential_and_bounded` 测试名 | safe |

## crates/kernel/src/nl/time.rs（24 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/kernel/src/nl/time.rs:33,42,67,74 | 魔数 200 出现 4 次（3 处 `(1..=200)` + 默认返回值），改上限要四处同改 | 提 `const MAX_TOP_N: usize = 200`，注释钉「=全局 MAX_ROWS」 | safe |
| crates/kernel/src/nl/time.rs:39-40 | `lower.find("top")` 不验前一个字符，英文词 `stop3`/`desktop5` 里的 "top" 会被误判成 TopN=3/5 | 命中后加判据 `pos == 0 |  |
| crates/kernel/src/nl/time.rs:57-71 | 最高级分支只用 `q.find(sup)` 看**第一次**出现，「最高…最好5个…」中第二次出现才带数字时漏判；与 L21「前」循环所有出现的策略不一致 | `find` 改 `match_indices` 循环，与「前」分支同形 | test |
| crates/kernel/src/nl/time.rs:119 | `s.split("")` + filter 空串是晦涩惯用法，且 D 表用 &str 逐字匹配 | 改 `s.chars()` + `&[(char, u32)]` 表，零分配零拐弯 | safe |
| crates/kernel/src/nl/time.rs:132-159 | `recent_n` 每个 lead 只查第一次出现：「最近销量，近7天呢」里「最近」后无数字 → `continue` 后「近」仍命中「最**近**」处的同一位置，句尾的「近7天」永远轮不到 | lead 内层改 `match_indices` 循环所有出现位置 | test |
| crates/kernel/src/nl/time.rs:156 | `n >= 1 && n <= 60` 与 L33/L42/L67 的 `(1..=N).contains(&n)` 风格不一致 | 统一为 `(1..=60).contains(&n)` | safe |
| crates/kernel/src/nl/time.rs:293 | `n - 1` 依赖 `recent_n` 的 1..=60 过滤才不下溢——非局部不变量，哪天 recent_n 放宽到 0 就 debug panic | 加 `debug_assert!(n >= 1)` 或注释钉住不变量来源 | safe |
| crates/kernel/src/nl/time.rs:147-149 | `starts_with('周') \ | \ | starts_with("个周")`、`starts_with('月') \ |
| crates/kernel/src/nl/time.rs:135 | `take(6)` 的 6 无出处注释（最长形态「三十五」+「个月」=5 字），后人容易随手改小 | 补一行注释说明窗口长度上界 | safe |
| crates/kernel/src/nl/time.rs:228 | `.expect(...)` + `.to_string()` 每个日期一次 String 分配，其实只需字节区间 | `dates` 存 `(at, range, date)`，最后一次性 `&q[at..at+10]` | safe |
| crates/kernel/src/nl/time.rs:302 | `chars().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect()` 双重反转收集，可读性差 | 先收集 `Vec<char>` 再切尾部 3 字 | safe |
| crates/kernel/src/nl/time.rs:303-307 | 序数两种来源语义不一：中文支取「一二三四数组优先级最先」（`position`），数字支取「head 里最后一个数字」（`chars().rev().find`）；head 同时含两种序数时取谁取决于类型而非位置 | 统一为「head 中最后出现的序数字」或注释声明刻意 | test |
| crates/kernel/src/nl/time.rs:384 | `cs[j..i].iter().collect::<String>().parse::<i32>()` 为 4 个 ASCII 数字一次堆分配 | 直接 `(cs[j] as i32 - 48)*1000 + ...` 手算，或对原串字节切片 parse | safe |
| crates/kernel/src/nl/time.rs:411-412 | `ym` 的 `month` 只容 1..=13，month=0 或 14+ 会生成 `'%Y-00-01'` 非法 SQL——不变量全在调用方 | `debug_assert!((1..=13).contains(&month))` | safe |
| crates/kernel/src/nl/time.rs:440 | 注释只说「『上个月』等相对词先排除」，实际 `"个月"` 同时挡住「前五个月/哪个月」这类未被任何规则承接的说法，行为是保守 None 但注释没写全 | 注释补一句「个月系一律交兜底/LLM」 | safe |
| crates/kernel/src/nl/time.rs:447 | 数字集合 `"一两二三四五六七八九十"` 缺 `零`，与 L25/L62/L138 的集合（含零）三处不一致 | 提一个 `const CN_DIGITS` 四处共用 | safe |
| crates/kernel/src/nl/time.rs:167 vs 461-467 | L167 模块注释列举「五条规则」停在相对词兜底，`rule_year` 是第六条却不在序列里；L461 自称「规则④·5」命名突兀 | 总注释补 rule_year 为规则④·5 并说明排序约束 | safe |
| crates/kernel/src/nl/time.rs:514-517 | `window_includes_today` 含单字「近」，「附**近**的门店」「接**近**」会误判当期 → `RequireTimeCap` 误造闸 | 单字「近」改判「近+数字/中文数字」前缀形态，或与 recent_n 共用判据 | test |
| crates/kernel/src/nl/time.rs:520-523 vs 529-534 | 文档写「问句里**第一个**时间表面词」，实现是 `PHRASES.iter().find()`——按**词表序**而非**句中位置**：「本月销量比上月」返回「上月」（表序在前） | 要么按 `match_indices` 取最小位置（改行为），要么把文档「第一个」改成「词表序首个」 | test |
| crates/kernel/src/nl/time.rs:526 | `q.chars().any(is_ascii_digit) && q.contains("20")` 前半冗余（"20" 必含数字）；且判据过宽：「门店20号本月销量」因含 "20" 整句拒绝继承 | 删冗余半句；要收紧则用 `explicit_year` 的形态判据 | safe |
| crates/kernel/src/nl/time.rs:529-532 | PHRASES 只有「近三个月」一种近 N 形态，而 recent_n 支持 1~60 全档——覆盖面不一致是刻意保守但没钉注释 | 注释说明「只继承唯一无歧义形态，其余不继承」 | safe |
| crates/kernel/src/nl/time.rs:91-94 vs 183-186 vs 471-476 | 「今天/今日/昨天/昨日」词组在 prev_window、yoy_window、rule_relative 抄了三遍——本仓反复批评的「两份实现必然漂」形态 | 提 `const TODAY_WORDS/YESTERDAY_WORDS` 或判定函数三处共用 | safe |
| crates/kernel/src/nl/time.rs:96 | 本月 prev 右端 `CURDATE() - INTERVAL 1 MONTH` 在月末压缩（3/31→2/28，当期 31 天 vs 上期 28 天），L86-89 的「逐档核过」没提这一折中 | 注释补一句月末压缩说明 | safe |
| crates/kernel/src/nl/time.rs:559 | 长注释与 `assert_eq!(tp("2025年的数"), ...)` 挤在同一行 | 换行 | safe |

## TracePanel.vue（23 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| TracePanel.vue:25 | `at?: string` 在模板中从未使用（事件级时间戳不展示） | 移除该字段，或在 route/retry 节点展示 fmtAt(ev.at) | safe |
| TracePanel.vue:35 | artifact 的 `id?: number` 从未使用（key 用的是 ei） | 移除字段，或用 `ev.id ?? ei` 做事件 key | safe |
| TracePanel.vue:53 | props.rounds 类型为必填 `TraceRound[]`，`?? []` 是死代码；且这层 computed 无额外逻辑 | 直接模板用 `props.rounds`，或把 prop 类型改为 `rounds?: TraceRound[]` 保留兜底 | safe |
| TracePanel.vue:90 | expanded 按轮下标记，而 L139 v-for key 用 `msg_id`；会话切换/列表刷新后展开状态滞留且可能指错轮 | watch(props.rounds) 清空 expanded，或改按 msg_id 记 | test |
| TracePanel.vue:100 | ms 为浮点时显示 `123.456ms` | 小于 1000 时 `Math.round(ms)` | safe |
| TracePanel.vue:107 | `hour12:false` 在部分浏览器午夜显示「24:xx」 | 改用 `hourCycle: 'h23'` | safe |
| TracePanel.vue:112 | stage/result 缺字段时渲染出字面量「undefined · undefined」 | 兜底：`STAGE_LABEL[...] ?? ev.stage ?? '路由'`、result 缺省时不拼「· xxx」 | safe |
| TracePanel.vue:113 | reason 缺失时显示「重试（undefined）」 | `?? '重试'` 兜底 | safe |
| TracePanel.vue:118 | 注释写「retry/interrupted 红」，nodeTone 并无 interrupted 分支（interrupted 是轮状态） | 修正注释，去掉 interrupted 或说明由 roundTone 处理 | safe |
| TracePanel.vue:126-128 | blocked/interrupted 与 failed 同红色，丢失「被拦截/中断」语义区分 | interrupted/blocked 用 warn 黄，failed/timeout 用 bad 红 | safe |
| TracePanel.vue:146 | at 为空时仍渲染空 `.tl-time` div 占位 | 加 `v-if="r.at"` | safe |
| TracePanel.vue:152-153 | 可点击的回答节点是纯 div，键盘不可达、无 aria-expanded | 加 `role="button" tabindex="0" @keydown.enter/space` 与 `:aria-expanded` | safe |
| TracePanel.vue:153+172 | 展开的 SQL `<pre>` 在节点内部，点击 SQL 文本（如想选中复制）会冒泡触发 toggleSql 收起 | `<pre>` 上加 `@click.stop` | test |
| TracePanel.vue:155 | 单字徽标「问/路/试/答/物」会被读屏逐字朗读，语义已由 tl-label 表达 | badge span 加 `aria-hidden="true"` | safe |
| TracePanel.vue:158 | tl-label 有 ellipsis 截断但无 title，长路由名截断后看不到全文 | 加 `:title="nodeTitle(ev)"` | safe |
| TracePanel.vue:163-167 | row_count/route/sql 全缺时渲染空 `.tl-detail` div | 外层加 `v-if="ev.row_count != null |  |
| TracePanel.vue:166 | 「SQL ▸/▾」文案过简，不知可点击展开 | 改为「展开 SQL ▸/收起 SQL ▾」或配合 aria-expanded | safe |
| TracePanel.vue:169 | 展示用 `{{ ev.title }}`，title 为空时渲染空链接；而 emit 侧已有 ` |  | '产物预览'` 兜底，两处不一 |
| TracePanel.vue:169 | href 仍是真实 URL，右键「新标签打开」/复制链接必 401（L49-50 注释自己说明），与拦截初衷相悖 | 改用 `<button type="button">` 或去掉 href | test |
| TracePanel.vue:184 | `.tl-count` 设了 flex:1 但文本左对齐，「N 轮」紧跟标题而非靠右 | 加 `text-align: right` 或去掉 flex:1 | safe |
| TracePanel.vue:202 | 仅 `--warning-text` 带 hex 兜底，其余 CSS 变量均无兜底，写法不一 | 统一带兜底或统一不带 | safe |
| TracePanel.vue:213 | `.tl-sql` 未设 monospace，SQL 用比例字体显示，与其他代码块惯例不符 | 加 `font-family: ui-monospace, monospace` | safe |
| TracePanel.vue:215-217 | <1100px 直接 `display:none`，窄屏用户完全无法看 trace | 改为可折叠抽屉/按钮唤起，而非直接隐藏 | test |

## warehouse_catalog.rs（23 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| warehouse_catalog.rs:390-396 vs main.rs:200-206 | `needs_sync` 用 `requested_tables()`（按小写表名去重），`mark_synced` 收的是探针 `stats.requested`（资产条数）——两个「requested」仅靠 :923 唯一性测试钉住才相等；一旦出现跨库同名表，标记永不匹配 → 每次启动全量探针 | 两端统一调同一函数，或在 `requested_tables` doc 写明该耦合 | test |
| warehouse_catalog.rs:415-427 | `version_marker(target, requested, rows.len(), 0)` 把 ds 作用域的 `rows.len()`（:417 按 `ds_id` 过滤）与数仓全局 requested 混进同一标记；多 ds 部署下永远不等 → 永久重同步 | doc 写明单 ds 前提，或标记两端统一作用域 | test |
| warehouse_catalog.rs:417 vs 425 | `table_name = ANY($2)` 大小写敏感，`default_ready` 却用 `eq_ignore_ascii_case`——大小写语义不一致；PG 中大小写不同的行使行缺失 → `default_ready` 永假 → 重同步死循环（:894 seed 的 UPDATE 同样大小写敏感、静默 0 行） | 两侧统一 `lower(table_name)` 比较 | test |
| warehouse_catalog.rs:408-431 | 三个否决因子（default_ready / comments_ready / marker）任一不满足即返 true，但无日志说明是哪扇门挡住——「为什么这次启动又全量探针」不可观测 | 返回 true 时 `tracing::debug!` 带三因子取值 | safe |
| warehouse_catalog.rs:449-451 | `validate_required_snapshot` 在 filter 闭包内对每个合同列 `to_ascii_lowercase()` 分配（每列一次 String） | 反向用 `available.iter().any( | c |
| warehouse_catalog.rs:535-552 | `ensure_snapshot_table` 在 save/load/probe 每个入口都发 `CREATE TABLE IF NOT EXISTS`；`load_snapshot`（:655-656）ensure+SELECT 两趟往返 | 用 `OnceLock`/`AtomicBool` 记住已建成，跳过重复 DDL | safe |
| warehouse_catalog.rs:570-575 | 排序比较器内每次比较做两次 `to_ascii_lowercase()` 分配（O(n log n) 次临时 String） | 改 `sort_by_cached_key( | t |
| warehouse_catalog.rs:586-604 | `canonical.push_str(&format!(...))`（:586,:598）每行一个临时 String | `use std::fmt::Write; write!(&mut canonical, ...)` 直写 | safe |
| warehouse_catalog.rs:588,591 | 同一表的小写 key 算两次（`canonical` 行一次、`get_mut` 一次，后者还分配 String 查表） | 每表 hoist 一个 `let key = table.name.to_ascii_lowercase()` | safe |
| warehouse_catalog.rs:630-633 | `stats.requested as i64` 等 4 处 `as` 截断式转换 | 改 `i64::try_from(...).unwrap_or(i64::MAX)` 或显式饱和 | safe |
| warehouse_catalog.rs:682-685 | `usize::try_from(x).unwrap_or_default()` 把 DB 里的负数/脏值静默变 0——吞错， degraded 统计被悄悄清零 | 负值时 `tracing::warn!` 或至少 `debug_assert!(x >= 0)` | safe |
| warehouse_catalog.rs:703-732 | `plan_fallback` 的 Reuse 分支不校验 `stored.version == VERSION`（:55 要求目录变化必须递增版本）：合同版本升级后 degraded 启动静默沿用旧版快照的 stats/snapshot_at | 版本不一致时 warn（轻量版）或视为无快照（严格版） | test |
| warehouse_catalog.rs:717-726,762-770 | Reuse 分支丢弃探针错误串，`tracing::warn!`（:765-769）只有 target/snapshot_at/stats，没有失败原因——运维无法诊断探针为何失败 | `FallbackPlan::Reuse` 携带 `probe_err` 并打进 warn 字段 | safe |
| warehouse_catalog.rs:807-809 vs 879-887 | `detail_layer` 大小写不敏感，`layer_rank` 大小写敏感（非大写 layer 得 0 分沉底）——两个 layer 帮手语义不一致，目前仅靠 :927 测试钉住大写 | `layer_rank` 内先 normalize，或注释写明依赖大写不变量 | safe |
| warehouse_catalog.rs:824-827 | `SPECIALIZED_CONTEXT` 的 `"pos"` 纯子串匹配：英文问句含 "post"/"purpose" 等即误判专门上下文，把默认事实 +40（:864-866）静默清零 | 换更长词形（如 "pos机"/"pos销量"）或词边界匹配 | test |
| warehouse_catalog.rs:833-863 | `windows` 不去重：问句重复短语时同一 n-gram 每次出现重复加分（如「销售额环比销售额」），分数被重复计数；去重还能缩小 57×W 内层循环 | `windows` 先过 `HashSet` 去重 | test |
| warehouse_catalog.rs:840-854 | 每资产每问重建 `corpus`（format!+lowercase ×57/问；调用点 recall/ods.rs:32 每问一次、deep_api.rs:3156 每报表一次），:854 的 `asset.domain.to_ascii_lowercase()` 同样每问每资产分配 | `OnceLock` 静态缓存小写 corpus 与小写 domain（内容编译期确定） | safe |
| warehouse_catalog.rs:859-863 | `word.chars().count().min(8)` 只依赖 word，却在资产循环内重算 57 次 | hoist 成问句级 `Vec<(&str, usize)>`（word, weight） | safe |
| warehouse_catalog.rs:843-852 | corpus 用单空格拼接 7 个字段，中文 n-gram 窗口可跨字段边界幽灵命中（前字段尾+后字段头拼出窗口词），最多 +8 噪音分 | 字段间用 `\n` 等不可能出现在窗口里的分隔符 | test |
| warehouse_catalog.rs:864-866 | `score += if specialized { 0 } else { 40 }` 的 `+0` 分支是无操作，掩盖「专门问题是不加权而非扣分」的意图 | 改 `if asks_default_sales && !specialized { score += 40 }` | safe |
| warehouse_catalog.rs:889-905 | `seed` 的 UPDATE 不检查 `rows_affected`：table_doc 缺行（同步跳过/失败）时静默 seed 空气，启动日志毫无痕迹 | `rows_affected()==0` 时 `tracing::warn!` 带表名 | safe |
| warehouse_catalog.rs:889-905 | 57 条顺序 UPDATE 无事务（每次启动经 seed.rs:18 执行）：中途失败留下半更新的 table_doc | 包一层事务（`sqlx::Transaction`），循环内用 `&mut *tx` | test |
| warehouse_catalog.rs:398-403,609,672 | `target.trim().to_ascii_lowercase()` 同一归一化在三处手写重复，漂移风险 | 抽 `fn normalize_target(&str) -> String` 统一引用 | safe |

## SqlAuditPanel.vue（22 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| SqlAuditPanel.vue:25-27 vs 182-186 | STATUS_LABELS 与 option 列表两处手写同一闭集，易漂移 | 由 STATUS_LABELS 生成 options | safe |
| SqlAuditPanel.vue:35-47 | authTail/authHeaders/errText 与 DataMapPanel 逐字重复 | 抽共享工具（同条） | safe |
| SqlAuditPanel.vue:107,111-112 | normCtx 对 chars/dropped/kept 直接 `Number()`，脏字符串 → NaN 直接显示在 UI | Number.isFinite 兜底 0 | safe |
| SqlAuditPanel.vue:119-121 | fmtMs 边界：999.6ms→「1000ms」、9999.9ms→「10.0s」（与 ≥10s 的「10s」小数位不一）、负 ms 显示负值 | round 后再判断档位，clamp ≥0 | safe |
| SqlAuditPanel.vue:124-127,203 | **shortAt 直接截取 UTC 钟点**：后端 `at` 是 `DateTime<Utc>.to_rfc3339()`（datamap_api.rs:980），显示时间与本地差 8 小时且无时区提示 | `new Date(at)` 转本地格式化，或标注 UTC | test |
| SqlAuditPanel.vue:132-156 | load() 无竞态闸：快速切状态过滤时两个在途请求后到者覆盖先到者（非发起序） | 请求序号或 AbortController | safe |
| SqlAuditPanel.vue:152 | `` `${e}` `` 拼 Error 对象，文案出现「Error: Failed to fetch」 | 取 `e instanceof Error ? e.message : String(e)` | safe |
| SqlAuditPanel.vue:158-160 | select 下拉展开时按 Esc，全局监听会先关整个抽屉 | 判断事件目标/composedPath | safe |
| SqlAuditPanel.vue:161-165 | 卸载不 abort 在途 load()，回调写已卸载组件 ref | AbortController | safe |
| SqlAuditPanel.vue:170 | dialog 无初始焦点，Tab 可跑出抽屉 | 挂载时聚焦关闭按钮 | safe |
| SqlAuditPanel.vue:177 | 关闭按钮内容 ✕ 充当可访问名 | 加 aria-label="关闭" | safe |
| SqlAuditPanel.vue:138,189 | 固定 limit=100，满 100 条时计数仍只显示「100 条」，无截断提示；响应里已有 count/limit 可判断 | 满额显示「已达 100 条上限」 | safe |
| SqlAuditPanel.vue:192 | loading 态无 role="status"（DataMapPanel L807 有，两处不一） | 补 role="status" | safe |
| SqlAuditPanel.vue:193 | error 态无 role="alert" | 补 role="alert" | safe |
| SqlAuditPanel.vue:194 | 「暂无审计记录」不区分过滤条件 | statusFilter 非空时「该状态下暂无记录」 | safe |
| SqlAuditPanel.vue:202 | 行点击展开但 tr 不可聚焦、无 aria-expanded，键盘用户无法展开 | 加 tabindex/Enter/Space 或行内嵌按钮 | safe |
| SqlAuditPanel.vue:208 | `title=row.sql` 把完整 SQL 塞进 tooltip，长 SQL 悬浮卡巨长 | title 截断或去掉（展开区已看全文） | safe |
| SqlAuditPanel.vue:211 | colspan="6" 硬编码，加列时静默错位 | 用常量或计算列数 | safe |
| SqlAuditPanel.vue:216 | 「{{ prompt_chars }} 字节」单位错：字段是字符数（chars），中文一字符多字节 | 改「字符」 | safe |
| SqlAuditPanel.vue:217 | 「裁掉 {{ trimmed.length }} 项」用的是分组数，不是被裁卡数（每组带 dropped） | 改「裁减 N 类」或对 dropped 求和 | safe |
| SqlAuditPanel.vue:223 | `{{ c.chars }}` 裸数字无单位 | 补「字」或 title 说明 | safe |
| SqlAuditPanel.vue:212 | 展开区无「复制 SQL」按钮，审计场景高频动作要手选复制 | 加复制按钮 | safe |

## tools/deep_contract_eval.py（22 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| tools/deep_contract_eval.py:6-7 | 用法示例只有 Windows `set` 写法 | 补 bash `export` 示例 | safe |
| tools/deep_contract_eval.py:2-10 | 头注释未提 `DMSAI_BASE`（21 行消费） | 补说明 | safe |
| tools/deep_contract_eval.py:22-25 | CASES 硬编码在脚本里，与 kb_eval/evaluation 的 JSON 题集模式不一致，且无注释说明理由 | 抽 JSON 或注释「题少故内联」 | safe |
| tools/deep_contract_eval.py:39 | `json.loads(raw)` 对空 body（如 204）抛 JSONDecodeError，不在任何 except 覆盖内，整趟崩 | `json.loads(raw) if raw.strip() else {}` | safe |
| tools/deep_contract_eval.py:46 | `TimeoutError` 是 `OSError` 子类，冗余 | 删 | safe |
| tools/deep_contract_eval.py:50-53 | env token 不预检；坏 token 会让每题 401 记成「题红」退 1，而非「门没开」 | 拿到 token 后探一次轻量端点，失败退 2 | test |
| tools/deep_contract_eval.py:60 | 登录失败不带服务端 error 摘要 | 附 `str(body)[:120]` | safe |
| tools/deep_contract_eval.py:68-74 | `payload.get("result") or {}` 等只挡 falsy；值是 list/str 时 76/88 行 `.get` 崩 | isinstance 守卫，非 dict 判红 | test |
| tools/deep_contract_eval.py:83 | 图表 kind 白名单仅 bar/line/pie，后端新增图表类型（scatter/组合图）会恒红 | 抽常量并注释「新增图表类型需同步」 | safe |
| tools/deep_contract_eval.py:89 | 「销售额」硬编码触发同比/环比校验，通用判定器里写死业务词，只对 DEEP01 有意义 | 挪进 CASES 配置（如 `"expect_comparisons": true`）或注释 | safe |
| tools/deep_contract_eval.py:101 | 内部编号正则只认 `\d{2}` 两位，三位编号（SEC-100）漏检 | 改 `\d{2,}` | test |
| tools/deep_contract_eval.py:103-105 | 「已验证」是普通中文短语，insight 正常措辞（「数据已验证无误」）会误红 | 收窄标记（如「内部已验证」「已验证✓」）或加前后边界 | test |
| tools/deep_contract_eval.py:108-121 | HTML 断言依赖 `class="sqlx"` 双引号精确串，模板改单引号/多 class 合并即假红假绿 | 换宽松正则 `class=["'][^"']*\bsqlx` | test |
| tools/deep_contract_eval.py:107,181-183 | preview_url 缺失时 html=""，107 行 `if html:` 使全部 HTML 断言静默跳过，只剩「缺可预览产物」一条，跳过无任何提示 | 显式记一条「HTML 结构断言未执行」 | safe |
| tools/deep_contract_eval.py:153 | selfcheck 通过文案列五项，但未覆盖「缺执行 SQL 清单/缺分析板块/缺可预览产物/缺 SVG/AI 不在 main 内/对比缺基期」分支 | 补对应断言 | test |
| tools/deep_contract_eval.py:166 | 「没有匹配用例」无 ❌ 图标，与兄弟脚本「❌ 一题都没匹配到」口径不一 | 统一措辞 | safe |
| tools/deep_contract_eval.py:176 | `token` 作第三位置参数传入 request(path, body, token, timeout)，可读性差易错位 | 改 `token=token` | safe |
| tools/deep_contract_eval.py:184 | preview_url 若为绝对 URL，`BASE + url` 拼接即坏，无约定说明 | 注释约定相对路径，或 urlparse 判断 | safe |
| tools/deep_contract_eval.py:194 | `payload["page"]` 直接索引，与 check_payload 内 `.get(...) or {}` 风格不一（当前靠前置判红兜住 KeyError） | 改 `payload.get("page") or {}` | safe |
| tools/deep_contract_eval.py:197 | 汇总只有「执行 N / 失败 M」缺通过数（kb_eval 汇总有「通过」列） | 补「通过 N-M」 | safe |
| tools/deep_contract_eval.py:198 | 全部用例 compose 失败（入口未落地）也退 1，与 kb_eval「门没开=2」归因哲学不一致 | 0 题实际评到时退 2 | test |
| tools/deep_contract_eval.py:175-196 | 逐题循环无进度输出，compose 默认 240s 超时下「在跑」与「卡死」不可分辨（evaluation.py 头注释 22-26 行自己写过这教训） | 每题开始打一行 `flush=True` 的进度 | safe |

## tools/registry_snapshot.py（22 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| tools/registry_snapshot.py:23 | `import settings as ts` 别名含义不显，且与 TypeScript 惯用缩写撞 | 改 `import settings as dms_settings` | safe |
| tools/registry_snapshot.py:56-59 | `psycopg2.connect` 无 `connect_timeout`，目标不可达时无限挂起无任何反馈 | 加 `connect_timeout=10` | safe |
| tools/registry_snapshot.py:54-57 | `u.port` 遇非法端口（如 `abc`、超界）抛裸 `ValueError` 栈 | 包 try，SystemExit「--pg-url 端口非法」 | safe |
| tools/registry_snapshot.py:57 | URL 查询串 `**q` 原样透传 connect kwargs，非法键（拼错的 `sslmod=`）裸 TypeError | 白名单过滤（sslmode/connect_timeout/application_name）或注释说明透传约定 | safe |
| tools/registry_snapshot.py:62-66,76 | 表不存在时 `columns_of` 返回空列表 → 拼出 `SELECT  FROM meta.x` 裸语法错误 | 空列时 SystemExit「meta.{table} 不存在或无任何列」 | safe |
| tools/registry_snapshot.py:75 | `'upload\_%'` 是无效转义序列，Python 3.12+ 报 SyntaxWarning | 改 `r'upload\_%'` 或 `'upload\\_%'` | safe |
| tools/registry_snapshot.py:75 | 排除 `upload_%` 数据源无任何注释说明原因 | 补一行注释（上传库元数据不随部署快照走之类的事实） | safe |
| tools/registry_snapshot.py:85 | `conn.close()` 不在 finally，导出中途异常连接泄漏 | try/finally 或 `contextlib.closing` | safe |
| tools/registry_snapshot.py:86 | 写快照非原子，中途崩溃留半截 JSON，导入侧才以 JSONDecodeError 发现 | 写 `path.tmp` 后 `os.replace` | safe |
| tools/registry_snapshot.py:93,98 | 导入不校验 `version`/`tables` 键，拿到别的 JSON 裸 KeyError | 开头校验并 SystemExit 清晰文案（version 不符先警告） | safe |
| tools/registry_snapshot.py:95 | `conn.autocommit = False` 是 psycopg2 默认值，冗余赋值 | 删除，或注释「显式事务、收尾统一 commit」表意 | safe |
| tools/registry_snapshot.py:102 | `row.get(c)` 缺列静默补 None，快照与代码列漂移时静默插 NULL | 缺键收集后统一告警 | safe |
| tools/registry_snapshot.py:104 vs 116 | 注册表去重用 `IS NOT DISTINCT FROM`、注释更新用 `=`，同文件两套空值语义（此处置空键不存在所以无害，但易被照抄出错） | 统一或在 116 行注释「键列均 NOT NULL」 | safe |
| tools/registry_snapshot.py:101-109 | 逐行 `cur.execute`，大快照 N 次网络往返 | `execute_batch`/executemany 分批 | test |
| tools/registry_snapshot.py:103-107 | check-then-insert 无唯一约束兜底，两人并发跑 import 会出重复行 | 注释「勿并发导入」或 `pg_advisory_xact_lock` | safe |
| tools/registry_snapshot.py:108 | `vals + [row.get(k) for k in key]` 键列在 cols 里已含一份，参数传两遍，读起来像 bug | 加一行注释说明后者是给 NOT EXISTS 条件的独立参数 | safe |
| tools/registry_snapshot.py:122-123 | 导入中途异常无 rollback/close，事务与连接悬挂 | try/except 里 `conn.rollback()`，finally `close()` | safe |
| tools/registry_snapshot.py:124 | 文案「10 分钟内向量自愈自动回填」偏保守：embed_fill.rs:1 是启动即跑一轮+每 10 分钟 | 改「服务启动即自愈一轮，最迟 10 分钟内补齐」 | safe |
| tools/registry_snapshot.py:105-108 | 新库新增 NOT NULL 无默认列时插入裸报数据库错，无表名/键值上下文 | execute 包 try，附 `meta.{table}` 与去重键值再抛 | safe |
| tools/registry_snapshot.py:132 | `--pg-url` 缺值时 `args[i+1]` 裸 IndexError | 显式检查并 SystemExit 用法提示 | safe |
| tools/registry_snapshot.py:134-135 | 多余位置参数被静默忽略（`export a.json b.json` 只用第一个） | `len(args)>2` 时报错退出 | safe |
| tools/registry_snapshot.py:141 | 用法说明打到 stdout；作为错误退出分支更应走 stderr | `print(__doc__, file=sys.stderr)` | safe |

## auth.rs（22 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| auth.rs:97,107,185 | 三处锁中毒策略不一致：`login_allowed`/`record_login`/`ip_rate_allow` 用 `.expect(...)` 直接 panic，而 `issue_from`(257) 优雅转 Err、`resolve_session`(279)/`revoke`(271) 用 `.ok()` 降级 —— 同一份文件两种姿态，限流器 panic 会打挂登录请求 | 统一成 `unwrap_or_else(PoisonError::into_inner)` 或全部优雅降级 | test |
| auth.rs:99,112 | 失败窗口魔法数：`5` 次、`300` 秒硬编码在逻辑里，本文件其余可调项（TTL_SECS/IP_RATE_*）都有具名常量 | 提取 `LOGIN_FAIL_MAX` / `LOGIN_FAIL_WINDOW_SECS` 常量并加钉值测试 | safe |
| auth.rs:95,106 | `login_allowed`/`record_login` 的 map key 是原始 login_name 无长度上限：喷洒唯一超长账号名可让 LOGIN_FAILS 无界增长（SESSIONS 有 1000 清扫、IP 限流有 CAP，唯独这张表没有任何帽） | key 截断（如 take(64)）或满员清扫过期项 | test |
| auth.rs:111 | `record_login` 失败时 `or_insert((0, now()+300))` 不重置已过期窗口：窗口过期后再失败只累加旧 `until` 已过的计数，靠下次 `login_allowed` 顺手删除才自愈，窗口语义漂移 | 失败分支先判 `until <= now` 则整体重置为 `(1, now()+300)` | test |
| auth.rs:211-216,258,280 | `now()` 每次调 `SystemTime::now()` 且同一次操作多次取（issue_from 一次、resolve_session 一次没问题，但 ip_rate_allow 与调用方各自取时间），且 `unwrap_or(0)` 让时钟回拨到 1970 前时所有限流窗口判定静默失真 | 保留现状可接受；至少在 `now()==0` 时不做限流判定或 debug 断言 | safe |
| auth.rs:259 | `map.len() > 1000` 魔法数且清扫后无硬帽：灌入 10 万个「活跃」会话时 retain 不删任何条，SESSIONS 无界涨（IP_RATE_CAP/CACHE_CAP 都有兜底，唯独会话表没有） | 提取 `SESSION_CAP` 常量；清扫后仍超帽时拒发新 token（Err）或淘汰最早过期者 | test |
| auth.rs:271 | `revoke` 用 `get_or_init`：在从未颁发过会话的进程里调 revoke 会白初始化一张空表 | 改用 `SESSIONS.get()`，None 直接返回 | safe |
| auth.rs:294-297 | `resolve()` 是全仓零调用的公开包装（grep 仅测试命中），与 `resolve_session` 重复 | 删除或 `#[cfg(test)]` 化；保留则需注释说明为谁预留 | safe |
| auth.rs:329-333 | `api_key_login` 命中后不 break（常量时间意图正确），但 mcp_keys 配置了两条相同 key 时后者静默覆盖前者，HashMap 迭代序不定 → 映射到哪个 login 不确定，无任何告警 | 配置加载处（或本函数 debug_assert）检测重复 key 并 warn | safe |
| auth.rs:435 | `strip_prefix(FEDERATED_ROLE_PREFIX).unwrap_or_default()`：上一行已 `starts_with` 判过，strip 不可能失败；`unwrap_or_default` 会在未来有人改判据时把 bug 静默吞成「空角色」 | 改 `.expect("starts_with 已判")` | safe |
| auth.rs:469 | `let roles: Vec<(i64, String)> = roles;` 是过滤逻辑删除后的残留空转绑定（上方长注释还在解释已删除的过滤） | 删掉这行，注释收进上方段落 | safe |
| auth.rs:502-505 | `verify_dms_token` 每次调用新建 reqwest::Client：丢连接池、每次 SSO 登录重建 TLS 上下文；xcx_api.rs:233 同进程已有静态 HTTP 客户端的正确范式 | 提取 `static DMS_HTTP: LazyLock<reqwest::Client>`（10s 超时） | safe |
| auth.rs:513-520 | 先 `resp.json()` 解析再查 `status.is_success()`：上游 401 回 HTML/空体时报「响应无效」而非「验真失败： HTTP 401」，错误分类误导排查 | 先判 status 再解析 body | test |
| auth.rs:507-517 | 三个 `map_err( | _ | ...)` 把 reqwest/serde 的真实原因全部丢弃且全程零日志；xcx 侧 `fetch_identity` 每个失败分支都 warn 留痕，SSO 侧完全瞎 |
| auth.rs:526-527 vs xcx_api.rs:154-160 | 同一个上游 getLoginInfo 的两份成功判据不一致：auth 要求 `code==0 && ok==true`，xcx 只看 `code==0` 完全不看 `ok` 字段 | 统一判据（或在 xcx 注释说明为何不查 ok） | test |
| auth.rs:12 | TTL 12h 纯滑动续期无绝对上限：定时探活可让一张 token 永久不过期（注释只说「对齐旧项目」，未声明这是有意取舍） | 注释中声明取舍，或加 `issued_at` 绝对过期 | test |
| auth.rs:171-173 | 文档声称 `api_wework_start / api_wework_login` 已首段接线 `ip_rate_allow`，实际 grep 全仓只有 api_sso(main.rs:1455)、api_login(1623)、xcx 三处接线 —— 企微两个公开端点零限流，文档超前于实现 | 补接线或修正文档清单 | safe |
| auth.rs:549 | 收口守卫 `read_dir` 非递归：`src/db/` 子目录真实存在，其下任何 `.rs` 直调 policy `load_principal` 都不会被守卫抓到 | 递归 walk（或显式列出子目录） | safe |
| auth.rs:563 | 守卫模式漏真实绕过路径：`use crate::dms_policy::principal;`（xcx_api.rs:57 现状，经 shim 无害）与 `dms_policy_core::principal::load_principal(`（真绕过）都不匹配现有两个字面量 | 模式扩为含 `dms_policy_core::principal` 与 `crate::dms_policy::principal` 的判定并区分 shim | safe |
| auth.rs:221-223,229-231 | `normalized_login`/`normalized_role` 对输入扫两遍 chars（count + any），O(2n) 无谓重扫 | 单遍 fold 同时计长与查控制字符 | safe |
| auth.rs:543,572-575 | 测试模块缩进错乱：543 行 doc 注释前多 4 空格、572 行 `#[test]` 顶格而函数体缩进异常（\r 行尾混入） | 统一缩进 | safe |
| auth.rs:208 | `client_ip` 对无头请求也走 `chars().take(64).collect()` 分配新 String，"unknown" 本可零分配 | 匹配分支直接返回 `"unknown".to_string()`，仅对真实 IP 做截断收集 | safe |

## llm.rs（22 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| llm.rs:93 | `/// 带供应商特有参数的构造` 文档注释 8 空格缩进，与周围 4 空格不符（格式化漏网） | 对齐缩进 | safe |
| llm.rs:108 vs 123-139 | `with_extra`（test）会 `trim_end_matches('/')`，生产路径 `with_conf_and_fallback` 不 trim：settings.json 里 base_url 带尾斜杠 → 请求打到 `//chat/completions` | 把 trim 收进 `with_conf_and_fallback` 或 `validate_base_url` 归一化 | test |
| llm.rs:486 | `validate_base_url` 只对 `base_url.trim()` 后的副本校验，原串（可能带首尾空白）原样存储并在 265/314 拼 URL——校验过的东西和用的东西不是同一个 | 构造时存 trim 后的值，校验与使用同源 | test |
| llm.rs:146,156,167,186,202,213,222,334 | `expect("llm runtime lock")`：锁一旦被毒化（persist 闭包 panic 等），此后**所有** LLM 调用连锁 panic | `unwrap_or_else(\ | e\ |
| llm.rs:201-203 | `conf()` 每次调用完整克隆 `Conf`（含 `extra` 整个 Map），`chat_with_usage`/`ChatModel::chat` 每次调用都付这笔 | `RuntimeConf.primary` 用 `Arc<Conf>`，快照克隆降为一次 Arc 计数以内的拷贝 | safe |
| llm.rs:270 | `.map_err(\ | _\ | anyhow!("LLM 请求失败"))` 吞掉 reqwest 真因（超时/DNS/TLS 不可分辨），排障只剩一句套话 |
| llm.rs:275-278 | `.json().await.map_err(\ | _\ | anyhow!("LLM 响应格式无效"))` 同样吞 serde/IO 真因 |
| llm.rs:272-274 | 非 2xx 只 bail 状态码，响应体直接丢弃且无一条 warn——供应商侧错误详情（限流原因、模型名下线）彻底消失 | 读受限长度 body（如 512B）打 `tracing::warn!`（不进错误链，红线不破） | safe |
| llm.rs:275 | `resp.json()` 无大小上限，异常上游/代理可回超大 body 吃内存 | 先查 `content_length()` 超限即拒，或 `bytes()` 限长后 `serde_json::from_slice` | safe |
| llm.rs:279-283 vs 328 | 文本路径不过滤空 content（空串原样 Ok 返回），视觉路径有 `.filter(!trim().is_empty())`——同文件两条出口不一致，空串会一路流到 `extract_sql` 才变成「无 SQL」 | 文本路径补同样的空串过滤，报「LLM 响应缺 content」 | test |
| llm.rs:333-334 | `vision_route` 在读锁里 `clone()` 整份 `RuntimeConf`（两份 Conf 全克隆），只为挑一条路由 | 锁内只取所需字段/引用再克隆单项 | safe |
| llm.rs:361 | `starts_with("https://")` 大小写敏感，`HTTPS://…`（合法 URL 写法）被拒 | 用 `reqwest::Url::parse` 后判 `scheme() == "https"` | test |
| llm.rs:373-381 | data URL 头匹配大小写敏感（`DATA:IMAGE/PNG;BASE64,` 被拒），RFC 2397 mediatype 本不敏感 | 头转小写后再 `matches!` | test |
| llm.rs:518,536 | `strip_thinking` 对每个响应无条件 `to_string()` 两次（518 全量克隆 + 536 trim 再克隆），无思考段时纯浪费 | 快速路径：不含 `<` 直接 `s.trim().to_string()` 返回 | safe |
| llm.rs:513-517 | `PAIRS` 大小写敏感，`<THINK>`/`<Think>` 变体剥不掉（思考草稿进 `extract_sql` 的风险通道留着一条缝） | 至少注释承认只钉小写形态；或小写化扫描后按原串切片 | test |
| llm.rs:61 | Display 文案「图片大小不能超过 16MB」，实际限的是 **URL 字符串字节数**（base64 比原图大 ~33%），用户 12MB 的图也会被拒 | 文案改「图片数据（base64 后）不能超过 16MB」之类 | safe |
| llm.rs:135 | 90s 超时是裸字面量 | 提 `const HTTP_TIMEOUT: Duration` 并注明取舍（Precise 档长生成） | safe |
| llm.rs:442-447 | `validate_provider_shape` 报错「extra_body 不许含保留或敏感字段」不说是哪个键，配置排障靠猜（键名本身不敏感） | 把命中的键名带进错误文案 | safe |
| llm.rs:449-451 | 文案「供应商地址与 fast/precise 模型不能为空」把地址与模型捆在一起，但此分支只在模型为空时触发（地址错误已在 448 返回），文案误导 | 改为「fast/precise 模型不能为空」 | safe |
| llm.rs:614 | `m.role == "system" && system.is_empty()`：首条 system 内容为空串时，第二条 system 会静默顶位——边界行为无注释无测试 | 改用显式 `seen_system: bool` 标志（语义更清晰） | test |
| llm.rs:990 | 源码闸切分 needle `"#[cfg(test)]\nmod tests"` 含 `\n`：CRLF 检出的工作树上 split 不中 → `.expect` panic，测试对环境行尾敏感 | needle 去掉 `\n`（如 corrector/insight 两文件按 `"#[cfg(test)]"` 切的形态），或先 `src.replace('\r', "")` | safe |
| llm.rs:997 | 断言 needle `"usage }"` 绑在当前 rustfmt 输出形状上，字段一重排/换行就假红 | needle 改成更稳的锚（如 `"ChatReply {"` + `"usage"` 分行各查一次） | safe |

## ctx.rs（22 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| ctx.rs:14-16 | 这三行是 LF 行尾，全文件其余为 CRLF（混合行尾），且与 8-9 行是两个分离的 std import 块 | 统一 CRLF，合并 `std::collections`/`std::sync`/`std::time` 进一个 std 块 | safe |
| ctx.rs:18-19 | `dms_connector::source` 排在 `dms_connector::mysql` 前，字母序反（rustfmt `reorder_imports` 会重排） | 交换两行顺序 | safe |
| ctx.rs:20-22 | `dms_kernel` 三条 use 可合并为一行（`{ChatRequest, ModelTier, ChatModel, ScopedSql}` + `llm::Usage`） | 合并 import | safe |
| ctx.rs:281 | `let RowSet { columns, rows, redacted, .. } = rs;` 的 `..` 正是注释（279-280 行）与测试（948 行）反复告诫的「新字段被静默丢掉」形态——RowSet 再加字段时编译器不会提醒 | 去掉 `..`，显式列全字段（编译期强制决策） | safe |
| ctx.rs:285,297 | `scoped.wire()` 调了两次（`sql` 字段与 `truncation_note` 各一次） | `let wire = scoped.wire();` 绑一次复用 | safe |
| ctx.rs:287 vs 741 | `truncated: row_count >= MAX_ROWS` 用 `>=`，`truncation_note` 用 `!=`（即 `==` 才出提示），738 行文档也只认 `==`：若 connector 某天返回 >MAX_ROWS，`truncated=true` 却没有续读提示，两字段互相矛盾 | 统一判据（两边都用 `>=`，并同步 738 行注释） | test |
| ctx.rs:331 vs 379-383 | `risk` 把 `supplemental.truncated` 算进 review 级，但 checks 文案只看 `r.truncated`——会出现「等级 review 却写着『结果未触发行数截断』」的自相矛盾凭证 | 截断 check 改为 `r.truncated \ | \ |
| ctx.rs:387,390 | 字面量 `"单据类型"` 出现两次（columns 与 Entity pairs 两处判据），改名时容易只改一处 | 提为 `const DOC_TYPE_COL: &str = "单据类型"` | safe |
| ctx.rs:414,495 | FNV 偏移基 `0xcbf29ce484222325u64` 硬编码两处 | 提为模块级 `const FNV_OFFSET: u64` | safe |
| ctx.rs:415-419,509-514 | `sql_fingerprint` 内联手写字节循环与 `fnv1a_feed` 完全同算法，两份实现 | `sql_fingerprint` 改为对每个 token 调 `fnv1a_feed(&mut h, token.as_bytes()); fnv1a_feed(&mut h, b" ")` | safe |
| ctx.rs:519,531 | `externalize_sql` 里 `sql.chars().count()` 扫了两遍全串（判据一次、format 一次） | `let total = sql.chars().count();` 复用 | safe |
| ctx.rs:539 | 表头 `columns.join(" \ | ")` 不按字符截：100 列长列名的小表会把整个表头灌进上下文（单元格有 `EXTERNAL_CELL_CHARS`，列名没有） | 列名同样 `chars().take(EXTERNAL_CELL_CHARS)` |
| ctx.rs:548-557 | 指针分支隐含不变量 `TABLE_EXTERNAL_ROWS > EXTERNAL_HEAD_ROWS`（否则 `take(5)` 行数与「其余 N-5 行」对不上），无任何编译期守卫 | 加 `const _: () = assert!(TABLE_EXTERNAL_ROWS > EXTERNAL_HEAD_ROWS);` | safe |
| ctx.rs:565 | `Value::String(s) => s.clone()` 先整串克隆再 `take(40)` 截断，长单元格白拷贝 | 直接 `s.chars().take(EXTERNAL_CELL_CHARS).collect()`，match 分支返回 `Cow` 或先 match 后截 | safe |
| ctx.rs:592-599 | recent 轮编号恒从 1 开始：有 early 被省略/摘要时，「最近对话」的第 1 轮实际是全历史的第 K+1 轮，模型引用轮号会歧义 | 编号从 `early_count + 1` 起 | test |
| ctx.rs:622 | 单轮问句截断长度 `200` 是无名魔法数（同文件其他阈值全是命名常量） | 提为 `const SUMMARY_TURN_QUESTION_CHARS: usize = 200` | safe |
| ctx.rs:630-631 | `user.chars().count()` 后又 `user.chars().take(...)`，两遍 O(n) 扫 | 计数一次存变量再判 | safe |
| ctx.rs:703-707 | `OnceLock<Mutex<HashMap>>` + `get_or_init` 手写初始化；rust 1.80+ 有 `std::sync::LazyLock` 更直白 | 若 toolchain 允许，换 `static CONTEXT_SUMMARIES: LazyLock<Mutex<HashMap<_,_>>>` | safe |
| ctx.rs:711-713 | `trace_id` 为空时静默 return，无任何留痕（连 debug 都没有）——装配侧传空键时排查无线索 | 加一行 `tracing::debug!("空 trace_id，跳过摘要暂存")` | safe |
| ctx.rs:717-721 | 爆帽驱逐 `map.keys().next()`：HashMap 无序，可能踢掉刚 stash、finish 还没 take 的新条目（注释已自知「丢任意一条」） | 注释保持；若在意公平性可换 `VecDeque` FIFO，但属行为变化——维持现状则把「可能踢新条目」写进注释 | test |
| ctx.rs:764 | `(0..b.len().saturating_sub(4))` 的 `4` 是 `b"limit".len() - 1` 的隐写 | `b.len().saturating_sub(b"limit".len() - 1)` 或局部 const，读的人不用心算 | safe |
| ctx.rs:772-775 | `tail_is_pure_limit` 把孤 `"offset"` token 也算合法（`LIMIT 200 OFFSET` 无数字尾部会被剥）——当前无实害，但判据与注释「尾部是纯 limit 子句」不完全吻合 | 改为成对消费：数字后若跟 `offset` 必须再跟一个数字 | test |

## schema.rs（22 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| schema.rs:38 | fn doc「三路召回：关键词强制补表（必入）+ trgm 相似排序补足到 k」漏了向量路（模块 doc L1 有三路） | fn doc 补「+ 向量近邻」 | safe |
| schema.rs:97-98 | 「word_similarity：短问句在长文档中的非对称匹配…」注释块挂在 `vector_tables` 里，描述的是 trgm 路——错位 | 挪到 `trgm_tables`（L144-149） | safe |
| schema.rs:9-10 | 模块 doc「forced=true，不占 k 的额度」名不副实：向量路 L128 `out.len() >= k` 把 forced 计入 k；trgm 净效果见下条 | doc 写准三路各自的额度口径 | safe |
| schema.rs:168,180 | 双判据交互：循环尾 L180 `out.len() >= k` 永远先于 L168 的 `k+forced` 触发——净效果是「forced≥1 且有候选存活时 trgm 恰好多推 1 张（与 F 无关）」，k+forced 余量永不成为实际停止条件；doc 钉了「两个判据都不许动」但没钉这个交互语义 | 先用无库单测钉住当前交互语义，再评审是否应为 k+forced | test |
| schema.rs:51-58 | `catalog_table_filter`：DMS 时每调用新建 57 个 String；一次 `retrieve` 建两回（L106、L151） | OnceLock 缓存或改 `Vec<&'static str>`（sqlx 可绑 text[]） | safe |
| schema.rs:73-77 | kw 空串行（ddl.rs:90 `keyword text PRIMARY KEY` 不拒 `''`）：`question.contains("")` 恒 true → 该行表被无条件 forced 进每轮 prompt | `if kw.trim().is_empty() { continue }` | test |
| schema.rs:77 | kw 不 trim：种子「销量 」（带空格）永不命中且零告警 | trim 后判 contains，或对不可命中行 warn 一次 | test |
| schema.rs:74-77 | `catalog_table`（DMS=57 项线性扫）排在便宜的 `contains` 之前；两判据无副作用 | 交换顺序：先 contains 后 catalog | safe |
| schema.rs:105 | `k.saturating_sub(1)`：k≤1 时向量路静默空转（LIMIT 0）无留痕 | `debug!` 一次「向量路额度为 0」 | safe |
| schema.rs:99-101 | embed=None 静默跳过向量路：与「向量路 0 命中」在日志不可区分 | `debug!` 一次 | safe |
| schema.rs:162 | `(k * 2) as i64` 无 saturating（k 来自调用方常量，仅记录） | `k.saturating_mul(2)` | safe |
| schema.rs:168 | `out.iter().filter( | c | c.forced).count()` 每轮循环重算；循环内不新增 forced:true → hoist 到循环前语义全等 |
| schema.rs:212-219 | DMS 路径 `warehouse_table_name` + `warehouse_qualified_table` 各自线性扫一遍 ASSETS（registry/mod.rs:182,186 都包 `warehouse_asset`） | 一次 `warehouse_asset` 兼得裸名+限定名 | safe |
| schema.rs:230,251-257 | DMS 分支只渲 contract，`table_doc.warn`（及 domain/comment）查了不用——与 L210「⚠️ 警告进表头注释（LLM 读 schema 必见）」承诺不符：DMS 表的 warn 静默不进卡 | DMS 表头补渲 warn（改 prompt 字节）或注释钉「DMS 走 contract 不含 warn」 | test |
| schema.rs:257 | 非 DMS 表头直接拼 `doc_warn`/`doc_comment`（K4 上传可控文本）：含换行则逃出 `-- ` 单行注释前缀，后续行以裸文本进 prompt；L293 只转义了尖括号 | 表头字段 `replace('\n', " ")` | test |
| schema.rs:269,273 | `cmt.replace('\'', "")` 算两遍（卡文本一次、语料一次） | 循环顶算一次复用 | safe |
| schema.rs:269,273 | 列注释不剥换行：`COMMENT '…'` 卡内文本与语料同带换行（与 L257 表头同族，列级） | 同剥换行 | test |
| schema.rs:267-275 | 末列后多一个逗号（每列跟 `",\n"` 后接 `");\n"`）：bare schema 不执行所以无害，但形态脏 | 拼列时用 join 或末列去逗号——改 prompt 字节 | test |
| schema.rs:229-250 | `render_schema` 每表 2 条顺序 await（table_doc/column_doc 互不依赖）；`retrieve` 每轮 ≤(k+forced)×2 次 RT | 两条 `tokio::join!`；进一步批量 IN 查询是更大改动 | safe |
| schema.rs:229-244 | 裸 String 解码 domain/warn/column_name 依赖 DDL NOT NULL（semantic/ddl.rs:45-46,79-82 是 NOT NULL）；老库若由更早 DDL 建表无此保证，一行 NULL → decode Err → `?` 整轮 retrieve 失败 | 注释钉 DDL 前提（或 SQL 侧 COALESCE） | safe |
| schema.rs:47-49 | `catalog_table(cx, t)` 包装只吃 `cx.ds`，收整个 cx 是多余间接 | 改收 `ds: &str` | safe |
| schema.rs:343-367 | 源码守测试 `.split("\n///")`（L349）依赖「函数体后紧跟 doc 注释」的排版约定——排版一变切段即歪，注释未声明该前提 | 注释钉排版前提 | safe |

## lineage.rs（22 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| lineage.rs:219-230 | `dtype_bucket` 与 datamap.rs:361 逐字重复（注释自认「散开两处」），datamap 侧修「point 误判」时这里极易漏改 | 提到 crate 内共享 `pub(crate) fn`，或至少在两侧测试互相钉同值 | test |
| lineage.rs:171-176 | `table_core` 用 `tokens.remove(0)` 循环剥前缀，每次 O(n) 搬移 | 改用起始下标后 `drain(..n)` 或直接迭代器 skip_while | safe |
| lineage.rs:194 | `HashSet<&String>` 多一层间接，`&str` 足够 | 改 `HashSet<&str>` | safe |
| lineage.rs:199-206 | `contains_table_name` 大小写敏感：目录文本若写 `T_goods` 或大写表名，直证信号静默丢失 | 先对 haystack `to_ascii_lowercase()` 再 match_indices（表名目录已小写） | test |
| lineage.rs:233-238 | `indexed()` 在 `column_overlap` 内每次重建两个 HashMap，`plan_edges` 里 O(high×ods) 对每对都重建 | 在 `plan_edges` 外层为每表预算 indexed 视图，`column_overlap` 保留 pub 签名包一层 | safe |
| lineage.rs:488-493 | 装载时表名小写化但**列名保持原样**，`column_overlap` 大小写敏感比对：'OrderDate'↔'order_date' 撞不上 | 装载时 `name.to_ascii_lowercase()`（STOP_COLS 过滤同受益） | test |
| lineage.rs:386 | `cols.get(h.table)` 在内层 ods 循环里每对重复查；且查找用原始大小写 h.table，与 489 行小写键只靠「目录全小写」约定对齐 | 外层循环先取 `hc = cols.get(&h.table.to_ascii_lowercase())` 一次 | safe |
| lineage.rs:397-399 | 每对为 joinable 查表分配 2-4 个 String（双向各一组） | joinable map 改用 `(&&str, &&str)` 键或在装载期预建双向索引 | safe |
| lineage.rs:429-440 | `ensure_edge_table_ready` 两次 `to_regclass` 查询两次往返 | 合并成 `SELECT to_regclass('meta.datamap_edge')::text, to_regclass('meta.idx_datamap_edge_uniq')::text` 一次 | safe |
| lineage.rs:481 | `names` 收全部 ASSETS 表（含不参与推断的 DIM/DWD 层），column_doc 查询多拉回无用行 | 只收 high ∪ ods 的表名 | safe |
| lineage.rs:490 | 列去重 `entry.iter().any(..)` 是 O（列数²) per 表 | 每表配一个 HashSet 记录已见列名 | safe |
| lineage.rs:512-521 | 逐行 upsert 无事务（同 datamap/usage 三处同形态） | 包事务 | test |
| lineage.rs:459-462 | lineage upsert 不刷 `last_seen`/`seen_count`，与 usage 写门口径不一（同 datamap.rs:829 条） | 统一语义后三处对齐 | test |
| lineage.rs:539-544 | `_ => report.by_overlap_weak += 1` 兜底：未来新增 base 串会被静默计入 weak 桶，报表失真无告警 | 三个已知串显式匹配，其余 `tracing::warn!` 并不计数 | safe |
| lineage.rs:503-509 | `tables_without_columns` 非空时只进报告，无 warn 日志；编排方不打印报告就无人知晓 | 非空时 `tracing::warn!("{} 张目录表在 column_doc 无列", ..)` | safe |
| lineage.rs:466-467 | 文档称「全部输入三次查询取齐」，实际还有 `ensure_edge_table_ready` 的 2 次存在性查询，共 5 次 | 注释改「三次数据查询 + 存在性检查」 | safe |
| lineage.rs:312 | `!s.overlap.shared.is_empty() && comment_ratio > 0.0` 中前者冗余（ratio>0 ⇒ 有共有列） | 删前一条件或注释说明防御意图 | safe |
| lineage.rs:363 | evidence 截断 20 是裸魔法数（测试 933 行钉着），无量名 | 提 `const EVIDENCE_SHARED_COLS_CAP: usize = 20;` | safe |
| lineage.rs:367-368 | `evidence_of` 里重算 `table_core(high.table)`/`table_core(source.table)`，`plan_edges` 算 name_match 时已算过一次 | 把 core tokens 作为 PairSignals 字段传入复用 | safe |
| lineage.rs:155 | `SUFFIX_TOKENS` 含 "fin" 与 `DOMAIN_TOKENS`（154）重叠，重叠是刻意还是笔误无注释 | 加一行注释说明「fin 既可作域前缀也可作后缀，双清单刻意」 | safe |
| lineage.rs:609-651 | `table_relations` 三条 SELECT 串行 await，彼此无依赖 | `tokio::try_join!` 并发（只读、同池连接需注意并发占用） | safe |
| lineage.rs:153-155 | 三份 token 清单变化「需评审」只有口头约定，无测试钉清单内容（阈值有钉点测试，清单没有） | 加测试钉住三清单长度/关键成员，改清单即改测试 = 评审触发器 | safe |

## SkillsPanel.vue（21 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| SkillsPanel.vue:14+16-17 | created_by/created_at/updated_at 声明后从未渲染（只用了 updated_by） | 删除字段，或在 meta 行展示 updated_at（格式化后） | safe |
| SkillsPanel.vue:36 | 注释称互斥锁管「行级操作」，但 save() 不在锁内，保存与 toggle 可并发 | 注释补充「save 不在此锁内」，或把 saving 纳入互斥 | safe |
| SkillsPanel.vue:74 | 列表项无逐项 shape 校验（id 缺失会 key 冲突），与 UsagePanel.vue:26 的 normalize 防御写法不一 | 过滤 `typeof it.id === 'number'` 的项 | safe |
| SkillsPanel.vue:77+123+149+174 | `${e}` 对非 Error 输出「[object Object]」（与 UsagePanel 同款问题） | `e instanceof Error ? e.message : String(e)` | safe |
| SkillsPanel.vue:146-147+121 | toggle 就地更新与 save 后的 load() 全量替换存在竞态：并发时 toggle 结果会被覆盖丢失 | toggle 期间禁用保存，或统一走 load() | test |
| SkillsPanel.vue:180+191 | Esc/点遮罩直接关闭，表单未保存内容静默丢失 | 有编辑内容时关闭前 confirm（或仅按钮关闭） | test |
| SkillsPanel.vue:196 | kicker「提示词包」与标题「Skills 管理」中英混用不一致 | 标题改「提示词包管理」 | safe |
| SkillsPanel.vue:197 | 「最长约 2 分钟」中「最长」与「约」矛盾 | 改「最长 2 分钟」或「约 2 分钟」 | safe |
| SkillsPanel.vue:197 vs 206 | 副文案「每包前 2000 字」与 placeholder「最多 20000 字」两个上限并列，未解释区别（注入截断 vs 存储上限） | 副文案补一句「20000 为存储上限，注入时截前 2000」 | safe |
| SkillsPanel.vue:197 | 「最多 5 包」限制在 UI 无任何当前计数/超限提示，启用第 6 包时无前端反馈 | 头部显示「已启用 n/5」 | safe |
| SkillsPanel.vue:199 | 关闭按钮无 aria-label（同 UsagePanel:115） | 加 `aria-label="关闭"` | safe |
| SkillsPanel.vue:205-206 | 输入框/文本域只有 placeholder 无 label，读屏不友好 | 加 aria-label 或 `<label>` | safe |
| SkillsPanel.vue:206 | 内容无 maxlength 也无字数计数，超限只能提交后吃后端错误 | 加 `maxlength="20000"` 或实时字数提示 | safe |
| SkillsPanel.vue:211 | 「新建缺省不启用…」提示在编辑模式下也显示，与场景无关 | 加 `v-if="editingId == null"` | safe |
| SkillsPanel.vue:224+229+230 | 禁用条件是 `busyId === s.id`（只禁本行），但 toggle/removeSkill 的守卫是 `busyId != null`（锁全表）；点其他行会静默无效且 checkbox 视觉翻回不修正 | 禁用条件改 `busyId != null`，与守卫对齐 | safe |
| SkillsPanel.vue:233 | title 原样放全文，最长 20000 字的 tooltip 会撑爆且难读 | title 截前 200 字 + 省略号 | safe |
| SkillsPanel.vue:157 | 原生 `window.confirm` 与全站自绘弹窗风格不一（低优先级） | 换自绘确认或保留并注释说明 | safe |
| SkillsPanel.vue:242-255 | 遮罩/对话框/头部/关闭按钮/加载态 CSS 与 UsagePanel.vue:148-158 近乎逐行重复 | 抽共享样式类或注释互相引用 | safe |
| SkillsPanel.vue:248 vs UsagePanel.vue:153 | sk-close 有 `flex-shrink: 0`，up-close 没有，同款按钮两处不一 | up-close 补上 `flex-shrink: 0` | safe |
| SkillsPanel.vue:245+258 | font-weight 750/650 非可变字体回退问题（同 UsagePanel） | 统一 700/600 | safe |
| SkillsPanel.vue:269 | 禁用按钮 `cursor: default`，不如果断的 `not-allowed` 表意 | 改 `cursor: not-allowed` | safe |

## query_log.rs（21 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| query_log.rs:55/58-60 | `conv_id` 无索引，而 trace_api `FAILED_SQL`（trace_api.rs:126）`WHERE conv_id=$1` 每开一次 trace 页全表扫一张只增表 | 加 `CREATE INDEX IF NOT EXISTS idx_query_log_conv ON meta.query_log(conv_id)` | test |
| query_log.rs:60 | `idx_query_trace` 对 `trace_id IS NULL` 的老行/未设关联键行也建索引项 | 改 partial index `WHERE trace_id IS NOT NULL` | test |
| query_log.rs:58-60 | 三条 CREATE INDEX 无存在性断言（455-456 只钉 status/context_summary 两列），索引被误删会静默退成全表扫 | 测试补三条索引名断言 | test |
| query_log.rs:16 | 「本 crate 单测 145 → 141」硬编码计数随测试增删立即腐烂 | 去掉数字，只留定性描述 | safe |
| query_log.rs:76-79 | `migrate` 多句不在事务中（同 chat.rs:53） | 包 tx | test |
| query_log.rs:76 + 451-457 | 幂等测试只查 `IF NOT EXISTS`，不防「注释内 ASCII 分号」把语句切碎（启动期才炸） | 加「每句以 CREATE/ALTER 开头」断言，把爆炸提前到测试期 | test |
| query_log.rs:120 与 204 | u32→i32 钳位（`.min(i32::MAX as u32) as i32`）写两遍 | 提 `fn clamp_u32_i32` | safe |
| query_log.rs:196 | `elapsed_ms.min(i64::MAX as u128) as i64` 与 `i64::try_from(..).unwrap_or(i64::MAX)` 等价，后者意图直白 | 替换 | safe |
| query_log.rs:183 | `e.to_string()` 只取 anyhow 最外层 context，connector 原始根因链丢失，失败行可查性差 | `format!("{e:#}")` 保链（单链形态逐字不变） | test |
| query_log.rs:221/230-235 | `let msg = e.to_string()` 在 typed `ConnectorError::Timeout` 命中前就已分配 | typed timeout 检查提到 `let msg` 之前（typed blocked 已在前），判据集合不变 | safe |
| query_log.rs:251 | warn 只有 `{err}`：无 trace_id/login/route 字段、不打错误链，故障时对不上是哪次问答 | 补结构化字段 + `{err:#}` | safe |
| query_log.rs:280-288 | context_summary UPDATE 失败时 warn 文案是「查询日志写入失败」，但主行其实已落库——误导排查方向 | UPDATE 单独捕获，文案区分「摘要贴回失败（主行已落）」 | safe |
| query_log.rs:282-286 | `UPDATE ... WHERE trace_id=$2` 不查 rows_affected：trace_id 撞键（重试复用）会一次改多行且无声 | `rows_affected() != 1` 时 warn | safe |
| query_log.rs:282 | 撞键场景另一解：贴回无幂等守卫，重试会覆盖先到的摘要 | 加 `AND context_summary = ''` | test |
| query_log.rs:85-86 | 「接它们要动 8 处签名，本轮不接」的「本轮」是时间性措辞，落地后即失所指 | 改「暂不接（成本/占比权衡）」 | safe |
| query_log.rs:259-276 | `.bind` 链与 `sqlx::query(INSERT_SQL)` 同缩进（4 空格），链式调用按惯例应再进一层（本机工具链未装 rustfmt，未验证 fmt --check） | 统一缩进 | safe |
| query_log.rs:271-272 | 空串→NULL 三元重复两遍 | 提 `fn non_empty(s: &String) -> Option<&String>` | safe |
| query_log.rs:325-326 | 连续两个空行 | 收成一个 | safe |
| query_log.rs:36 与 chat.rs:33 | DDL 一个是模块级 `const DDL`，一个是函数内局部 `let ddl`，同 crate 两种风格 | 统一为模块级 const | safe |
| query_log.rs:119-123 | `tokens()` 两次 Relaxed load 非原子快照，并发 `add` 时 prompt/completion 可能读到不同代 | 观测场景无实害，补一句注释说明可接受 | safe |
| query_log.rs:225 | `msg.contains("无权访问数据源")` 不锚定：报错文本混入该短语（如问句原文被回显进 connector 报错）会误判 blocked | `starts_with` 锚定，或补一条「误伤」测试钉住边界 | test |

## usage_api.rs（21 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| usage_api.rs:117,125,132 | 三处 `err(500, e)` 透 sqlx 原生错误，且与本文件 :312 已用的通用文案自相矛盾 | warn + 通用文案 | safe |
| usage_api.rs:319-325 | 缓存**读**失败直接 500 端点挂掉：缓存是优化不是正确性（:333 注释自己说的），写失败只 warn、读失败却致命，纪律不一致 | 读失败降级为 miss 继续生成 + `tracing::warn!` | test |
| usage_api.rs:111-132 | `usage_block` 三条 SQL 串行 await | `tokio::try_join!` | test |
| usage_api.rs:158-161 | admin 时本人块、全局块两个 `usage_block` 串行（共 6 条串行查询） | 两个 block 也 `try_join!` | test |
| usage_api.rs:115,118 | 聚合查询 `fetch_optional + unwrap_or` 死分支 | `fetch_one` | safe |
| usage_api.rs:155-157 | `load_principal` 任何错误（含 auth MySQL 宕机）都映射 403"身份或角色不可用"：DB 故障报权限错误，误导且排障方向错误 | 区分：查无此人 403 / DB 错误 500 | test |
| usage_api.rs:305-307 | 同上，`sample_questions` 里同一模式 | 同上 | test |
| usage_api.rs:162-168 | 两次 `body.as_object_mut().expect("usage_block 恒返对象")` 重复取、重复 panic 消息 | 取一次 object 连续两个 `insert` | safe |
| usage_api.rs:168 | `p.login_name.clone()`：`p` 此后不再使用，clone 多余 | move `p.login_name` | safe |
| usage_api.rs:308 | `Viewer::new(p.login_name.clone(), vec![p.role_code.clone()])`：`p` 此后不再使用，两处 clone 多余 | 两次 move | safe |
| usage_api.rs:217-225 | `cache_parse` 用 `saturating_sub`：缓存 `at` 在未来（时钟回拨/脏数据）时差为 0，被视为新鲜直到未来时刻到期，可长期钉住旧问题集 | `at > now` 也按 miss 处理 | test |
| usage_api.rs:240-243 | `trim_start_matches` 把"数字+符号"当前缀无差别剥：合法问题"2026年预算怎么定？"被削成"年预算怎么定？" | 只在"数字串后紧跟分隔符（./、/)/空格）"时才剥前缀 | test |
| usage_api.rs:273 | `rsplit_once('.')` 去扩展名无长度校验：文件名" v1.2报销制度"（点在中部非扩展名）被截成"v1" | 仅当点后缀 ≤5 字符且为字母数字时才剥 | test |
| usage_api.rs:277 | 模板轮换用 `enumerate` 的 `i`：怪名被 `continue` 跳过后轮换错位（无害但产出随跳过数漂） | 改 `out.len() % TEMPLATES.len()` | safe |
| usage_api.rs:353,374 | `list_docs` 与 chunks 查询 `.unwrap_or_default()` 静默吞错、零日志：端点"绝不报错"是对客户端的纪律，排障侧该 warn 没 warn | 两处各加 `tracing::warn!(err=%e, ...)` 再降级 | safe |
| usage_api.rs:366-374 | `CHUNK_SQL` 无 LIMIT/ord 过滤：每篇只用前 2 块，却把 6 篇文档的全部 chunk 拉回应用层（大文档可达数千块） | SQL 侧 `c.ord < $4` 或 LATERAL 每篇 `LIMIT 2` | test |
| usage_api.rs:332-343 | 缓存 miss 无 singleflight：并发 N 个请求各打一次 20s LLM（KV_SET 是 upsert，写侧安全，纯浪费） | 加单飞，或至少在头注写明已知 stampede | test |
| usage_api.rs:332-343,360-362 | `"empty"`（无可见文档）结果也按 24h 缓存：空间刚上传文档后最长 24h 样例仍为空 | 空结果不缓存或用短 TTL | test |
| usage_api.rs:395-400 | fallback 产出为空（文档名全是怪名）时 `source` 仍报 `"fallback"`，与 `"empty"` 语义混 | fallback 空则改报 `"empty"` | test |
| usage_api.rs:412 | `user.push_str(&format!("文档《{name}》摘录：\n"))` 每次循环一次无谓临时分配 | `write!` 或三次 `push_str` | safe |
| usage_api.rs:94-97 | `is_admin` 与 `admin_api::admin`（admin_only 内部）判据是两份独立实现，管理员定义漂移只靠单测钉、无代码复用 | 复用同一函数，或两文件注释互相指引 | safe |

## embed_fill.rs（21 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| embed_fill.rs:24,33 | `INTERVAL=600s` 与日志文案「10 分钟后重试」两处独立硬编码，改常量文案即失真 | 文案泛化为「下轮重试」或由 `INTERVAL` 生成 | safe |
| embed_fill.rs:30-31 | `Ok(0)` 完全静默：「没活干」与「锁被别的实例持有」在日志里不可区分 | 加一条 `tracing::debug!` 区分两种早退 | safe |
| embed_fill.rs:52 | unlock 失败被 `let _ =` 吞掉：连接带着会话级锁还回池，该连接终身占锁（注释 :42 自己描述过此坑） | unlock 失败至少 `warn!`；进一步可 `conn.close()` 弃连接 | safe |
| embed_fill.rs:61-62 | `list_datasources` 不过滤 status（datasource.rs:81 全量返回），disabled 源每轮白跑 `null_vec_rows` | 收集 ds_ids 时过滤 `status=="active"` | test |
| embed_fill.rs:63-71 | 每 (ds_scoped target × ds) 一次 `null_vec_rows`，全量健康时仍每 10 分钟 3N+1 次空查询 | 先跑一次全局 `EXISTS(embedding IS NULL)` 短路 | test |
| embed_fill.rs:66 | 任一 ds/target 失败 `?` 中断整轮，剩余 ds 与 `fill_kb` 全部跳过 | 按 target 粒度 catch 记录后继续，末尾汇总 | test |
| embed_fill.rs:83 | 魔数 1000（文本截断字符数）无命名无出处注释 | 提常量 `TEXT_CHAR_CAP` 并注明与离线配方对账 | safe |
| embed_fill.rs:83 vs 109 | meta 侧截 1000 字、kb 侧 `row.text` 不截：同为 embed 输入两条截断策略且无注释说明差异 | 补注释（chunk 长度已由 ingest 定界）或统一 | safe |
| embed_fill.rs:90 | `with_context("{t:?} embed 服务缺席")` 不带 ds_id，ds_scoped 失败时分不清哪个源 | context 加 `ds` 字段 | safe |
| embed_fill.rs:87-90 vs 110 | embed 缺席语义两处不对称（一处 `?` 报错、一处静默跳过），各自有意但无交叉注释，易被判为疏漏 | 两处互加一行注释指认对方 | safe |
| embed_fill.rs:98-101 | 逐行 `write_vec`，N 行 N 次 RTT | 批写（`UPDATE ... FROM UNNEST` 或显式事务） | test |
| embed_fill.rs:110 | kb 侧 embed 缺席静默跳过，连 debug 都没有，与 meta 侧留痕不对称 | `else { tracing::debug!(...) }` | safe |
| embed_fill.rs:111 | ensure 文案「kb embed 返回条数不符」缺实际/期望数，:92-96 meta 侧带了 | 对齐文案补数量 | safe |
| embed_fill.rs:32 | info 只有总数 n，无 target/ds 分布，排障时分不清补的是哪类 | 日志带分 target 计数（或 debug 明细） | safe |
| embed_fill.rs:129 | `flip_embedded_docs` 每轮无条件执行（含 embed 缺席、无新块时），刻意与否无注释 | 加注释说明刻意每轮对账，或 `n==0 && rows.is_empty()` 时跳过 | safe |
| embed_fill.rs:80-94 | `update_sql` 五分支全是静态串却返回 `String`（93 行统一 `.to_string()`），`write_vec` 每行调一次 | 改返回 `&'static str` | safe |
| embed_fill.rs:49-78 | `select_sql` 每次调用 `format!` 重建（`ds_pred(1)` 结果恒定） | `LazyLock` 缓存五份成品 | safe |
| embed_fill.rs:98-112 | `null_vec_rows` `limit` 未夹紧，负值 PG 报错 | `limit.max(0)` | safe |
| embed_fill.rs:115-130 | `write_vec` 每行重建 update SQL 串（配合上一项可归零分配） | 同上 | safe |
| embed_fill.rs:18-20 | 注释「调度每 10 分钟叫一次」把调度间隔写死在本文件，间隔定义在 server 侧，改一处即腐 | 注释改为互指 server 调度点 | safe |
| embed_fill.rs:35-36,156-159 | `ALL` 手写 5 项、测试再手写 4 项目标清单，新增目标漏加无守卫 | 测试改遍历 `MetaVecTarget::ALL` | safe |

## present.rs（21 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| present.rs:26 | `specs.iter().map(...).collect::<Vec<_>>().join("")` 中间落一个 `Vec`，可直接 `collect::<String>()` | 去中间 Vec | safe |
| present.rs:47,93 | `let n = name;` 是无意义别名（后面全用 `n`，没有影子/收窄） | 直接用 `name`，删别名 | safe |
| present.rs:62 | `n.contains('数') \ | \ | n.contains("销量") \ |
| present.rs:60 | `contains("SKU") \ | \ | contains("sku")` 漏掉 `Sku`/`sKu` 等大小写混合 |
| present.rs:148 | `if items[0].delta.is_none()` 在 delta 全部算完之后才判：已有 delta 时 130-147 行的计算全白做 | 把 is_none 判据提到函数体最前（early return） | safe |
| present.rs:161-165,214,332-337 | `build` 已算过 `index_roles`，`mk → compute_insight` 里又全量重扫一遍 metric/cat/time 下标（214、219-220 行） | `mk`/`compute_insight_on` 改收 `&RoleIdx` 复用 | safe |
| present.rs:170 | `chrono::Local::now().date_naive()` 用**应用进程本地时区**判「本月」，而注释（200 行）声称与 SQL 侧 `CURDATE()` 同口径——容器若 UTC、业务东八区，月初/月末当天两边日期差一天，不足月判定错 | 显式固定业务时区（如 `FixedOffset::east_opt(8*3600)`）或与 DB 时区配置同源 | test |
| present.rs:175 | `ROW_CAP = 200` 复刻 `dms_agent::MAX_ROWS`（注释自知「只能复刻数值」），纯约定无编译期/测试期联动，改一边就漂 | 把上限常量下沉 kernel 两边共用；或加跨 crate 测试断言相等 | safe |
| present.rs:207 | `rows[0].iter().all(...)` 对**空行**（`vec![[]]`）`all()` 恒真 → 空列空行也报「没有数据」，与 368 行空 KPI 块叠加出怪形态 | 判据补 `!rows[0].is_empty()` | test |
| present.rs:235,277-278,376,392-393,435 | 全函数族直接索引 `&r[mi]`/`rows[0][i]`/`&r[y]`：DB 路径行宽恒等于列数没问题，但 `build` 是 `pub` 纯函数，锯齿行（某行比 columns 短）会当场 panic；236 行取 cat 却用了安全的 `r.get(ci)`，同函数内两种风格 | 统一改 `.get(i)` + `unwrap_or(&Value::Null)` 类兜底，或函数头 `debug_assert` 行宽一致 | test |
| present.rs:244-261 | 排行洞察不过滤负值：`vals` 含负指标（毛利额为负的月份/客户）时 `total` 被负值压低，`top/total*100` 能算出 >100% 的「占比」（饼图分支 435 行有 `all_nonneg` 守卫，洞察分支没有） | 负值存在时只出「榜首/前三合计」不出百分比，或过滤负值并注明 | test |
| present.rs:222-226 | `unit` 的 Percent 分支 `format!("{:.1}%", v)` 默认 0-100 口径：毛利率族是小数比值（130-136 行 patch_kpi_delta 特意 ×100 处理），趋势洞察里 0.1963 会渲染成「0.2%」而非「19.6%」 | `unit` 里对 `is_ratio_percent_label(label)` 的值 ×100（需把 label 传进闭包） | test |
| present.rs:252-256 | 截断提示用 `vals.len()` 说「前 {} 项」，但 `vals` 过滤了不可解析行，`vals.len()` 可以 < 200，「截断为前 193 项」与事实（截断为前 200 行）不符 | 文案改报 `rows.len()`（截断行数）或改措辞「可解析项」 | test |
| present.rs:281 | `pct >= 0.0` 时 `dir = "增长"`：首末完全相等会输出「整体增长 0.0%」 | 加 `pct.abs() < eps` → 「持平」分支 | test |
| present.rs:319 | 注释「34 项线性扫描，行数上限 50 时无所谓」：province_cn 的消费点是排行洞察（232-243 行），那里行数上限是 ROW_CAP=200，不是 50（50 是图表 BAR_MAX） | 注释改为「行数上限 200」 | safe |
| present.rs:368 | `ix.metric.is_empty() \ | \ | ix.metric.len() != specs.len()`：空 columns + 单行（`build(&[], &[vec![]])`）时两条件都假，守卫通过 → 产出 `Kpis { items: [] }` 空 KPI 块 |
| present.rs:385-395 | `entity` 先 `.filter( | (i,_) | !matches!(rows[0][*i], Null))` 再 `.map( |
| present.rs:430-443,446-455 | `one_cat_one_metric`/`grouped_bar` 无 `rows` 非空守卫：0 行结果（行宽正常、0 行数据）会落到 Pie/Bar 空图块而不是纯表格（前面 kpis/entity/detail/trend 全被行数挡掉） | 守卫补 `rows.is_empty() → None`，让空结果落 `Block::Table` | test |
| present.rs:95-97 | 时间判据 `ends_with("date")\ | ("time")` 大小写敏感，`下单DATE`/`EndTime` 不命中（中文关键词不受影响） | 只对 ASCII 尾巴做 `to_ascii_lowercase` 后再判 |
| present.rs:101 | Id 判据 `ends_with("code")\ | ("_id")` 同样大小写敏感（`ORDER_ID` 不命中） | 同上 |
| present.rs:214 | `metric_idx` 收集整个 `Vec<usize>` 只为判 `len() != 1` 并取首项 | `filter(...).take(2)` 迭代器手数两个即可，省一次分配 | safe |

## crates/agent/src/answerers/business_lookup.rs（21 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/agent/src/answerers/business_lookup.rs:100 | `exact_literal(code)?` 对注册表正则已识别的单号再校验，失败会硬 Err 整个问答（mod.rs:41 Err 原样上抛）而不是回落——一个 60+ 字符单号就能把问句打成 500 | 失败改 `Ok(None)` 回落，或注释「注册表形状保证不可达」 | test |
| crates/agent/src/answerers/business_lookup.rs:118-130 | 明细族循环串行 await（设备族 2 张明细表 = 2 个串行 RTT），彼此无依赖 | `futures::future::join_all` 并发（保持 executed 顺序），配测试 | test |
| crates/agent/src/answerers/business_lookup.rs:119 | `detail_policy` 未覆盖的明细表被 `continue` 静默跳过——注册表声明了明细却不查，无任何留痕 | 加 `tracing::warn!(table=%detail.table, "明细表无点查策略，跳过")` | safe |
| crates/agent/src/answerers/business_lookup.rs:153 | `exact_literal(value)?` 对 entity_query(471 行）已校验过的值再 bail——不可达 Err；若是防御，缺一句注释说明 | 注释「上游已 valid_entity_value，此行纯防御」 | safe |
| crates/agent/src/answerers/business_lookup.rs:186 | `table_result(cx, sql, rows, 10)`：SQL 自带 LIMIT 1，limit 参数 10 永不触发，纯误导读者 | 传 1 或给 limit 参数改名/注释 | safe |
| crates/agent/src/answerers/business_lookup.rs:192-194 | 「匹配到多个候选」分支是死代码：两类实体 SQL 都 LIMIT 1，`row_count > 1` 恒假——留着让人以为有多候选处理 | 删除死分支（或去掉 LIMIT 1 让它复活），配测试 | test |
| crates/agent/src/answerers/business_lookup.rs:207,228 | `main_sql.clone()`（207）进 table_result，228 行又拼一次 `{main_sql};\n{sql}`——同一串克隆两回 | 先拼好 combined_sql 再一次性 move | safe |
| crates/agent/src/answerers/business_lookup.rs:207 | `usize::MAX` 当 limit 魔法值表「永不截断」，读者要去查 table_result 实现才懂 | 提常量 `const NO_TRUNC: usize = usize::MAX;` 或注释 | safe |
| crates/agent/src/answerers/business_lookup.rs:212-215 | `visible_customer_codes(cx)` 每次调用重新 filter+clone 整个 Vec；本函数与 retain_visible_rows(355)、row_visible(332) 一轮问答多次重复构建 | answer 入口算一次往下传（或挂 cx 缓存） | safe |
| crates/agent/src/answerers/business_lookup.rs:311 | `row_visible` 在 match 之前无条件取 customer_code 并在 scope_visible 里对 Customer 白名单做线性扫——Employee/FailClosed 可见性根本不用 customer，白扫 | 把 customer 求值挪进需要的分支（或 scope_visible 内惰性化） | safe |
| crates/agent/src/answerers/business_lookup.rs:353-368,421 | `retain_visible_rows` 每行都 `value_at` → `columns.iter().position(...)` 线性找列下标，O（行×列） | 闭包外先算好 customer_code/employee 列下标各一次 | safe |
| crates/agent/src/answerers/business_lookup.rs:396-397 | `scope_visible` 的 `AccountBillManager(_) => false` 与 `FailClosed => false`：row_visible 已在 match 上层拦截 AccountBillManager，这两个臂只对未来误用者生效且无任何提示 | 臂上加注释「row_visible 已分流，此臂防误用」或 debug_assert | safe |
| crates/agent/src/answerers/business_lookup.rs:416-428 | `value_at` 与 `cell_by_name` 是同一段「列名→下标→取值」逻辑的两份抄写 | 合并为一个 helper（RowSet 版 + columns/row 版互为委托） | safe |
| crates/agent/src/answerers/business_lookup.rs:447-457,486-497 vs entity.rs:27-37 | 客套前缀/尾巴词表与 entity.rs 的 LEADING_INTENT/TRAILING_INTENT 是大比例重复的两份（各自漂移风险：entity 有「看看」族、这里没有） | 提到共享常量文件，两处引用 | safe |
| crates/agent/src/answerers/business_lookup.rs:499-505 | `valid_entity_value` 对 `value.chars()` 起两趟 `any` 扫描（禁字符一趟、控制字符一趟） | 合并成一趟 `any` | safe |
| crates/agent/src/answerers/business_lookup.rs:554 | `merge_rowsets` 用 `columns.contains(column)` 去重，O（列²)；列少无感但 HashSet 版一样短 | 换 `HashSet<&str>` 判重 | safe |
| crates/agent/src/answerers/business_lookup.rs:603-610 | identity pairs（单据类型/单号/主表/明细表）与 header 列 zip 出的 pairs 直接拼接：header 投影里若含单号列，「单号」在头卡出现两次 | extend 前过滤与 identity 同 label 的列，配测试 | test |
| crates/agent/src/answerers/business_lookup.rs:622-623 | `present::build(&rows.columns, &rows.rows)` 算完 blocks 立刻被 623 行整体覆盖——build 的 blocks 计算白做（columns/interact 仍被用） | 手工构造 ViewSpec{columns,..} 或注释说明只借 build 算 columns | safe |
| crates/agent/src/answerers/business_lookup.rs:628 | `truncated = rows.rows.len() >= 50` 拿**合并后**行数对**单表** LIMIT 50 判：两个明细各 30 行 → 60 ≥ 50 → 误报截断 | 按「任一单表行数 ≥ 50」判（在循环里记 flag 透传），配测试 | test |
| crates/agent/src/answerers/business_lookup.rs:643 | `family.details` 为空时 join 出空串，头卡出现「明细表：（空）」 | 空时给「（无）」占位，配测试 | test |
| crates/agent/src/answerers/business_lookup.rs:681 | `columns: columns.clone()`——解构出的 `columns` 此后再无人用，整 Vec 克隆可省 | `columns` 直接 move | safe |

## sales_fact.rs（21 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| sales_fact.rs:53-65,105 | 找到表分支 `table.name.clone()`，随后每个缺失列又 `table_name.clone()`（最多 12 次相同 String 克隆） | 用 `iter().position()` 拿索引避免首次克隆，缺失列循环复用同一份 clone | safe |
| sales_fact.rs:58-64 | 注释幂等键是「包含当前 TABLE_COMMENT 子串」：版本更替后旧警示文本永远残留并继续累加新文本，表注释单调膨胀 | 用稳定标记定位并替换旧警示段，而非纯子串判断 | test |
| sales_fact.rs:235-240 | `unit()` 用 `""` 当「无单位」哨兵，调用方 registry/mod.rs:459 还在 `unit.trim() == metric.unit()`，哨兵语义靠约定 | 改 `Option<&'static str>`（无单位返回 `None`），同步两处调用点 | test |
| sales_fact.rs:356-360 | `Predicate::contains(dim, "")` 生成 `INSTR(expr,'') > 0`，MySQL/Doris 对非 NULL 行恒真 → 静默匹配全表；而 `eq(dim,"")`（:352）恒假，两构造器空值语义相反 | 构造器对空串 debug_assert/返回 `Option`，或至少在 doc 写明清规 | test |
| sales_fact.rs:362-371 | `one_of` 不去重 values（重复值生成冗余 IN 项），也不过滤空串元素（`IN ('')` 恒不命中，调用方易误解） | doc 写明「调用方保证非空无重复」，或内部 dedup+过滤空串 | safe |
| sales_fact.rs:425-429 | 手写 `Default` impl：`&[T]` 与 `Option` 均实现了 `Default`，可直接派生 | 换 `#[derive(Default)]`，删手写 impl | safe |
| sales_fact.rs:431-433 | `quote` 只转义 `\` 和 `'`，控制字符（`\0`/`\n`）原样进 SQL；`\0` 会被连接器/DB 拒绝，变成运行期错误而非构造期发现 | `debug_assert!` 无控制字符，或 doc 写明值域约定 | safe |
| sales_fact.rs:436-438 | `dimension_names()` 每次调用分配 `Vec`，与同文件 :44 `contract_columns()` 的 `impl Iterator` 风格不一致 | 同样返回 `impl Iterator<Item = &'static str>`（唯一调用点 seed_defs.rs:251 仍走 collect） | safe |
| sales_fact.rs:451,452,456,538 | 魔板字面量 `"{} >= "`、`" AND {} < "` 在 4 处重复硬编码，kernel 模板格式微调时容易漏改其中一处 | 抽 `const`（如 `const AND_END: &str = " AND {} < "`）统一引用 | safe |
| sales_fact.rs:456-467 | `explicit_end` 只在 YEARWEEK 分支（:473-475）被消费；`DATE({}) =`/`YEAR({}) =`/QUARTER 分支静默丢弃已解析出的显式右端——kernel 未来若产出带右端的年/日模板，右端被静默忽略成完整周期 | 分支未消费 `explicit_end` 时返回 `None`（调用方回落），或统一拼接 | test |
| sales_fact.rs:485-500 | 进行中周期词表（本月/本周/今年/本季度…）是 kernel 时间解析器词表的影子副本：kernel 加新词（如「当季」）时此处不跟随 → 模板命中却不截断右端，未来日期脏数据混入 | 注释交叉引用 kernel 词表位置，或词表上移 kernel 导出共用 | safe |
| sales_fact.rs:502-511 | `has_explicit_month` 每次调用 `collect::<Vec<char>>()` 堆分配，仅为按下标取前一个字 | 改 `char_indices()` 零分配遍历 | safe |
| sales_fact.rs:513-517 | `has_explicit_quarter` 只检查第一个「季度」（`question.find`）；「本季度和三季度对比」这类第二个「季度」才是显式的问句被误判为进行中 → 右端错误截到今天 | 用 `match_indices("季度")` 检查全部出现 | test |
| sales_fact.rs:534-544 | `comparison_time_bounds` 靠 `template.contains(" AND {} < ")`（:538）推断「单日模板」；doc（:532-533）说单日不再扩一天，但这层耦合未写明——kernel 若给单日模板加显式右端，逻辑静默反转 | doc 点明「单日模板不得带 `AND {} <`」这一耦合，或加 debug_assert | safe |
| sales_fact.rs:547-553 | `metric_subquery` 子查询复用同一别名 `sf`；当前唯一注册点（seed_defs.rs:197）独立使用故正确，但一旦嵌入同样以 `sf` 为别名的外层查询即成阴影别名，排查困难 | doc 写明「不得嵌入 `sf` 外层查询」，或子查询换独立别名 | safe |
| sales_fact.rs:590-593 | 第二个 assert 恒真：`Dimension` 是封闭枚举且 `DIMENSIONS`（:255-264）列齐全部 8 个变体，`contains` 永远成立——死断言 | 删除该 assert，或替换为真正的不变量（如下条的维度唯一性） | safe |
| sales_fact.rs:594-602 | 无重复维度/指标防护：`dimensions`/`metrics` 含重复时生成重复 `` AS `名` `` 别名与重复 GROUP BY 表达式 → Doris 重复列错误或歧义结果；当前靠调用方自觉 | assert 两者各自唯一（或在构建时 dedup） | test |
| sales_fact.rs:605,643 | 两处 `predicate.0.clone()` 每个谓词一次 String 分配，仅为 join | 收集 `Vec<&str>` 再 `.join(" AND ")`（slice of &str 直接支持） | safe |
| sales_fact.rs:620-625 | 排序键与 SELECT 列表无关联校验：`Sort::dimension(d)` 而 `d` 不在 `dimensions` 时，GROUP BY 下 ORDER BY 非分组表达式 → Doris 运行期报错（排序键维度未入选） | 装配时校验 sort key ∈ dimensions ∪ metrics，违例 assert | test |
| sales_fact.rs:581-631 | doc 未写 `limit.clamp(1, 1000)`（:628）的静默截断；截断行为目前只有测试间接钉住 | doc 补「limit 被钳制到 [1,1000]」 | safe |
| sales_fact.rs:633-657 | `detail_sql` doc 未提 `limit.clamp(1, 500)`（:655），也未提 `ORDER BY ... ABS(sf.amount) DESC`（:653）按金额绝对值排序的口径 | doc 补齐钳制区间与排序口径 | safe |

## docker/parser/Dockerfile（20 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| docker/parser/Dockerfile:28 | `FROM python:3.12-slim` 浮动 tag（patch 级漂移），与 L45 自己立的「版本钉死」原则相悖 | 钉 `python:3.12.x-slim-bookworm` 或 digest | test |
| docker/parser/Dockerfile:45 | 注释声称「版本钉死」，但 apt 层（libreoffice L39-41、tesseract L69-72）版本随 Debian 源浮动——口径不诚实 | 注释补一句「pip 钉死、apt 随 Debian 浮动（distro 包无法钉）」 | safe |
| docker/parser/Dockerfile:51-52 | 注释「`_cap_ocr` 探 `PIL.Image`/`pytesseract`/`tesseract` 三样」已与代码不符：embed_service.py:708-723 的 `_cap_ocr` 现在是千问 flash 优先——有 `DMS_QWEN_OCR_KEY`+PIL 即判可用，根本不看 tesseract 三样 | 改注释为「千问 key 或 tesseract 三样，二有一即可」 | safe |
| docker/parser/Dockerfile:26-27 | 注释假设「去掉 -calc/-impress 后 `_cap_legacy` 会照实报不支持」不实：`_cap_legacy`（embed_service.py:725-729）只探 soffice 二进制存在 + 目标格式解析器，探不到 calc/impress 组件缺失——只装 writer 时 .xls/.ppt 会自报可用、转换时才炸 | 修正注释（写明该假设未验证），或给 `_cap_legacy` 补组件级探测 | safe |
| docker/parser/Dockerfile:39-41,69-72 | 两个 RUN 各做一次 `apt-get update`，pip 也分两处（L46、L72）：层划分按叙述不按失效域，多一次 update、多一层，改 tesseract 还连带重跑 pip | apt 合一层、pytesseract 并入 L46-48 主 pip 层（叙述注释可保留分段） | safe |
| docker/parser/Dockerfile:46-48 | pip 钉版本但无 hash 校验——L45 的理由（镜像是可分发产物）同样适用于供应链完整性 | 加 `--require-hashes` 或至少注释承认未做 | safe |
| docker/parser/Dockerfile:46,72 | pip 以 root 运行，构建日志必有 `WARNING: Running pip as the 'root' user` 噪音 | `ENV PIP_ROOT_USER_ACTION=ignore` | safe |
| docker/parser/Dockerfile:74 | 拷了 `make_fixtures.py` 却没拷 `make_silent_fixtures.py`——后者靠 parser.ps1:165 运行时挂 `/mk` 才进容器；同性质探针脚本两种分发方式，无任何注释说为什么 | 两个都 COPY（parser.ps1 可省 `/mk` 挂载），或注释说明取舍 | safe |
| docker/parser/Dockerfile:74 前 | 无 `ENV PYTHONDONTWRITEBYTECODE=1`：容器内每次跑 python 都在 /app 下写 `__pycache__` | 加该 ENV | safe |
| docker/parser/Dockerfile:78 | CMD 用了 `-u`，但探针的一次性容器（parser.ps1:151、166、187 跑 make_fixtures/make_silent/parse_probe）都没带 `-u`，print 缓冲导致日志延迟/乱序 | 加 `ENV PYTHONUNBUFFERED=1`，一处覆盖所有调用方 | safe |
| docker/parser/Dockerfile:76 | 注释说端口「同形」，但没提宿主侧默认发布的是 8078（parser.ps1:27 默认 `-Port 8078`）——单读 Dockerfile 的人会去敲宿主 8077 | 注释补「宿主侧默认发布 8078（8077 被宿主 embed 服务占着）」 | safe |
| docker/parser/Dockerfile:77 | `EXPOSE 8077` 硬编码，而 parse_service.py:34 支持 `PARSER_PORT` 覆盖，改端口后 EXPOSE 成误导 | 注释一句「EXPOSE 仅文档，以 PARSER_PORT 为准」 | safe |
| docker/parser/Dockerfile:43-44 | 注释说「pypdf 是 BSD 兜底」「三级降级」，但漏点 fitz 这一级（embed_service.py:694-696 实际是 pymupdf4llm→fitz→pypdf）；fitz 来自 pymupdf 传递依赖，砍包的人不知道它会一起没 | 注释补「fitz 是 pymupdf4llm 带入的中间档」 | safe |
| docker/parser/Dockerfile:12-15 | 红线注释叮嘱「别 COPY settings」，但对运行时唯一的入口环境变量 `PARSE_ROOTS`（parse_service.py:40，默认 `/kbdata:/tmp`，含 /tmp 这个放宽项）只字未提——镜像自文档缺这块 | 头注补一行 PARSE_ROOTS 语义与「别为了探针放宽到 /app」的警示 | safe |
| docker/parser/Dockerfile:30-32 | 「587 个包 / 160 个 / 186 个」实测数字无日期戳（对照 L17 有「本机 2026-07-29」），同类数据一处可复核一处不可 | 补日期戳 | safe |
| docker/parser/Dockerfile:19 | 「+501MB」与「/usr/lib/libreoffice 280MB」两数并列无解释差值（其余 ~220MB 是依赖库），读者困惑 | 注释补一句差值构成 | safe |
| docker/parser/Dockerfile:1-6 | SAC 拦 lxml 的叙事在本文件、parse_service.py:19-22、parser.ps1:8-10 三处各写一遍且细节略异，易漂 | 留一处全文，另两处改为互链 | safe |
| docker/parser/Dockerfile:74 | build context（docker/parser/）内无独立 `.dockerignore`——根 .dockerignore 只对仓库根 context 生效；当前 context 仅 4 文件无害，将来往里放临时夹具会静默进 daemon | 加 `docker/parser/.dockerignore` 或注释提示 | safe |
| docker/parser/Dockerfile:78 前 | 无 HEALTHCHECK：/health 端点现成（parse_service.py:117），parser.ps1:134-143 只能外部轮询 30 次 | `HEALTHCHECK CMD python -c "import urllib.request;urllib.request.urlopen('http://127.0.0.1:8077/health',timeout=3)"` | safe |
| docker/parser/Dockerfile:全文 | 无 `USER`：LibreOffice headless + tesseract 均 root 跑 | 非 root 用户（/tmp 与 kbdata 挂载写权限需实测） | test |

## mcp_api.rs（20 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| mcp_api.rs:70 | `RETRYABLE_EN` 里 `"connection"` 与 `"connect"` 并存，前者是后者的超集，恒为冗余分支 | 删 `"connection"` 保留 `"connect"` | safe |
| mcp_api.rs:72 | `to_ascii_lowercase()` 无条件执行，中文文案命中 CN 分支时白算一次全文小写 | 先判 CN，未命中再算 `low` | safe |
| mcp_api.rs:68,73 | `"连接"`/`"connect"` 子串过宽：`"连接配置缺失"`、`"connection string invalid"` 这类永久错误被误判为可重试（与 65 行「宁少不多」的偏向相悖） | 换成更具体的瞬时短语（"连接超时"/"连接重置"/"connection refused | reset |
| mcp_api.rs:96-98 | `req_id` 接受小数/负数 id（`is_number()`），JSON-RPC 明确 SHOULD NOT 带小数部分 | 改判 `i.is_i64() \ | \ |
| mcp_api.rs:107 | id 存在但类型非法（如 `id:true`）时也报「缺 id」，文案误导排障 | 文案改「缺 id 或 id 类型非法（仅收字符串/数字）」 | safe |
| mcp_api.rs:129-135,137-213 | `server_info()`/`tools()` 每次请求重建整棵 JSON（含 5 次 `format!`），内容全静态 | `std::sync::OnceLock<Value>` 缓存一份 clone 出去 | safe |
| mcp_api.rs:290,317 | 未知 method/工具名原样回显进错误消息，客户端可塞超长字符串撑大响应 | 回显前 `chars().take(64)` 截断 | test |
| mcp_api.rs:300-310 | `call()` 先 `load_principal`（一次 DB 往返）再 match 工具名，乱填工具名的请求白打一次身份库 | 先校验工具名在闭集内再加载 principal | test |
| mcp_api.rs:347,354,368 | 内部错误 `e.to_string()` 原文回给外部 MCP 客户端（可含库名/SQL 片段）；artifact_api 的 `db_err` 是固定文案，两文件口径不一 | 对外固定文案 + `tracing::warn!` 记原文 | test |
| mcp_api.rs:358 | `out.unwrap_or_default()`：序列化失败被吞，客户端收到成功响应体却是 `"null"` | `map_err` 成 `(EXEC_FAILED, "结果序列化失败")` | safe |
| mcp_api.rs:372-373 | `hits.iter()` 后 `json!` 逐字段 clone（doc_name/text 等 String 全复制一遍） | `hits.into_iter()` 直接 move | safe |
| mcp_api.rs:413,431 | 关键字小写化做两遍：调用方 431 行已 `to_lowercase`，`node_matches` 413 行又做一次 | 删 431 行的重复小写化（函数已自带归一） | safe |
| mcp_api.rs:731 | 测试注释说「纯函数约定入参已小写」，与 412-413 行代码注释「大小写归一化收在这里」及实际行为直接矛盾 | 改测试注释与代码对齐 | safe |
| mcp_api.rs:480 | 用 `edge_kind_filter("")` 取「全部 kind」是隐式契约，读代码的人要猜空串语义 | 加一行注释或在 datamap_api 提供 `all_edge_kinds()` | safe |
| mcp_api.rs:482 | `&["pending".to_string()]` 每次调用堆分配一个 Vec+String | 抽 `fn pending_statuses() -> Vec<String>` 或在共用层接受 `&[&str]` | safe |
| mcp_api.rs:499-501 | `json_text` 序列化失败静默兜底 `"{}"`，对 `json!` 产物本不可达，但失败时无任何信号 | 兜底分支加 `debug_assert!` 或 `tracing::error!` | safe |
| mcp_api.rs:286-291 | 不支持 `ping`：MCP 规范有 ping，保活型客户端会拿到 -32601 | 加 `"ping" => Ok(json!({}))` 臂 | test |
| mcp_api.rs:326,328 | 用户显式传的 `ds` 不进 `triage`（恒 `DMS_DS_ID`）：若 triage 用 ds 取词表/ schema 线索，显式选源被静默忽略 | 确认 triage 第三参语义；若有影响则传 `ds.as_deref().unwrap_or(DMS_DS_ID)` | test |
| mcp_api.rs:156,375-377 | kb_search 描述只列 `doc_id/doc_name/page/heading_path/score`，实际 payload 还有 `chunk_id/ord/text`，契约文档不全 | 描述补全字段清单 | test |
| mcp_api.rs:259,269-273 | 鉴权失败 warn 只有 `key_len`，没有客户端地址；handler 未取 `ConnectInfo`，撞库时无法溯源 | handler 加 `ConnectInfo<SocketAddr>` 并入日志 | safe |

## daily_digest.rs（20 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| daily_digest.rs:23,117 | 同 embed_fill：`INTERVAL` 与「10 分钟」文案双写 | 同上 | safe |
| daily_digest.rs:26 vs 27-46 | 「+8 时区」有 `FixedOffset::east_opt` 与 SQL 内 `'Asia/Shanghai'` 两套表达，语义同（无 DST）但无互指注释，漂移风险 | 两边注释互指 | safe |
| daily_digest.rs:48-50 vs 27-31 | 「今天」两个来源：应用时钟 `business_today()`（KV 标记）与 PG 时钟（artifact 判定）；应用/库时钟或容器 TZ 不一致时标记与实物错位，触发重复生成 | 统一从库侧 `SELECT (now() AT TIME ZONE ...)` 取今日 | test |
| daily_digest.rs:130 vs 183 | run_round 的 `today` 在拿锁前算，`generate` 内重新 `business_today()`；跨午夜一轮可能 KV 写昨天、产物算今天，下轮重复生成（靠 prune 兜底） | 把 `today` 作为参数传入 `generate` | test |
| daily_digest.rs:27-31 | `TODAY_DIGEST_SQL` 的 `ORDER BY id DESC` 对存在性判断多余（只需 EXISTS），且 :714 测试把这个多余排序钉死了 | 改 EXISTS 短查询并同步松测试 | test |
| daily_digest.rs:32-46 | `PRUNE_DAILY_SQL` 内联重复「今日最新 id」子查询两段（自身一段 + 与 TODAY_DIGEST_SQL 同形一段），维护时要同步改多处 | CTE 收敛为一份 | test |
| daily_digest.rs:135-138 | 短路路径 `prune_daily` 失败 `?` 使整轮 Err warn：清理失败不该让「今天已出过」变成告警噪音 | 清理失败降级 warn + 返回 Ok(false) | test |
| daily_digest.rs:140-161 | 锁连接占用期间 `st.owned.fixed(...)` 走池内其他连接；若池上限被配成 1 将自锁死，无任何提示 | 注释约束或启动断言 `pool_size >= 2` | safe |
| daily_digest.rs:162 | unlock `let _ =` 吞错（同 embed_fill:52） | 失败 warn | safe |
| daily_digest.rs:201-212 | 11 个互不依赖的查询全串行 `await`，整轮耗时=总和 | `tokio::try_join!` 并发 | test |
| daily_digest.rs:237-258 | 19 行 `vec![Value::from(..), Value::from(..)]` 样板，阅读噪音大 | 局部 helper 闭包 `row(name, val)` | safe |
| daily_digest.rs:247-249,444-446 | `orders_y as f64` 等 i64→f64 转换重复 6 处；>2^53 有理论精度损失无注释 | 提变量复用 + 一行注释 | safe |
| daily_digest.rs:272-273 | `md.replace("<!--AI-->", ...)` 只换第一处；report_md 若将来出现第二个占位符会静默漏填 | 断言 `md.matches("<!--AI-->").count()==1` 或注释 | safe |
| daily_digest.rs:380-391 | `change()` 用 `f64::EPSILON`(≈2.2e-16) 做零判定：金额级小基线（0.001）会算出天文百分比 | 换业务阈值（如 1e-6） | test |
| daily_digest.rs:388 | 双零分支返回 `"0.0%"` 与正常分支 `"{:+.1}%"` 的 `"+0.0%"` 符号不一致 | 统一带符号输出 | test |
| daily_digest.rs:469-480 | `section` 闭包内 5 次 `push_str(&format!(...))` 临时 String | `write!` 宏直写 | safe |
| daily_digest.rs:477 | 维度名只 `replace(' | ')`；含 `\n`/`\r` 的脏维度值仍会撑破 markdown 表 | 一并替换换行为空格 |
| daily_digest.rs:506 vs 483 | 图题「昨日 TOP5 客户」与小节题「昨日 TOP5 客户（不是门店）」不一致，判官口径强调的「非门店」在图上丢了 | 图题对齐 | test |
| daily_digest.rs:300-304 | `Row` 六元组位置与 `s.kpis` 列序隐式耦合；sqls() 调列序即静默错位，:643-674 测试只钉子串不钉顺序 | 测试补列序断言 | test |
| daily_digest.rs:115 | info「今日份已生成」缺 data_day/耗时字段，对账不便 | 日志带 `data_day` | safe |

## insight.rs（20 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| insight.rs:120 | `times` 用 `.cloned().collect::<Vec<String>>()` 把每条时间条件字符串克隆一遍，仅为喂给 `join_conds(&[String])` | `Vec<&String>`/`Vec<&str>` 借用，`join_conds` 签名放宽到 `&[impl AsRef<str>]` | safe |
| insight.rs:139,160 | `insight()`/`insight_deep_for()` 内部各调 `self.caliber()`，而 `server/src/insight_api.rs:110` 已算过一次——每请求 ast 解析+三遍文本扫描 ×2 | 开私有 `insight_with_caliber(&self, llm, caliber: &str)`，handler 把算好的串传进去；公开签名不变 | test |
| insight.rs:236 | 传输错误与 `content=None` 被 `.ok()?`/`.content?` 静默吞掉无 warn——「模型挂了」这个最值得留痕的分支反而没日志（240 行 warn 只覆盖空串/网址） | 改 `match`，Err/None 分支各补 `tracing::warn!` | safe |
| insight.rs:240 | 「{what}被丢弃（空 / 含链接）」两种原因共用一条文案，日志里分不清是空还是含链接 | 拆成两条 warn 文案 | safe |
| insight.rs:251-252 | `matches("\\n").count()` 全扫一遍，`replace` 再全扫一遍 | 用 `match_indices` 取第 2 处位置即返回，或 `replacen` 计数 | safe |
| insight.rs:262-263 | `has_url` 为 4 个 ASCII needle 付一次 Unicode `to_lowercase()` 全串分配 | `to_ascii_lowercase()`（needle 全 ASCII，语义等价）；或先查 `"]( "` 命中再 lowercase | safe |
| insight.rs:276 | 每行 `map(cell).collect::<Vec<_>>().join(" \ | ")` 一次中间 Vec 分配 | 循环直接 `push_str` 进 `body`，行间插 `" \ |
| insight.rs:287 | 字符串单元格 `s.clone()` 原样进简报：含 `\n` 的单元格会把「一行一条记录」的表格形状撑裂，模型看到的行列错位 | `cell()` 内把 `\n`/`\r` 替换为空格 | test |
| insight.rs:278-279 | 只有 `row_count > n` 才印行数说明：`rows` 为空但 `row_count > 0`（调用方不回传明细行）时简报只有表头，模型以为零数据 | 补 `rows.is_empty() && row_count > 0` 分支：「（共 N 行，本次未回传明细）」 | test |
| insight.rs:405-438 | `#[cfg(test)] where_frag` 把 341-398 的扫描器复制了 33 行（含独立 END 常量 L407），两处漂移无任何测试会发现 | 测试改写为断言 `where_frags`（取唯一元素），删掉副本 | safe |
| insight.rs:341-398 | `where_frags` 不处理 `--`/`#`/`/* */` 注释与反引号引号——`distinct_exprs` 在 476-477 行有同款 ponytail 坦白注释，这里没有，后人会以为它防住了 | 补一段同风格 ponytail 注释 | safe |
| insight.rs:460 | `"INTERVAL "` 带单个尾空格：`INTERVAL\t30`、`INTERVAL  30` 漏判（455 行注释已声明假阴只丢高亮，但此处收窄无代价） | 改 `"INTERVAL"`（词边界由函数名语境保证） | test |
| insight.rs:468 | `0..b.len().saturating_sub(6)` 的 6 是「窗口 7 减 1」的魔数，读者要自己推 | `b.windows(7).any(\ | w\ |
| insight.rs:517-521 | `clip` 先 `chars().count()` 全扫再 `chars().take(n)` 重扫 | `let mut c=s.chars(); if c.by_ref().nth(n).is_some()` 单扫 | safe |
| insight.rs:116,127,279,511 | `push_str(&format!(...))` 每次产生一个临时 String（compound.rs:117,177 同款） | `use std::fmt::Write; write!(s, ...)` | safe |
| insight.rs:194-211 | 四个词表数组（device_story/unsupported_risk/unsupported_strategy/cross_window_conflict）每次调用在栈上重建，风格与模块级常量 `SUPERLATIVE`（compound.rs:53）不一致 | 提为模块级 `const` | safe |
| insight.rs:460-462 | 时间函数关键字数组同样每次调用重建 | 同上提为 `const` | safe |
| insight.rs:138-141 与 159-162 | `insight()` 与 `insight_deep_for()` 的 hits 构造两段逐字重复，只差 `brief`/`brief_n` | 抽 `fn hits(&self, n: usize) -> Vec<Hit>` | safe |
| insight.rs:97 | 文档注释只说「简报本来就只取前 BRIEF_ROWS 行」，深度版实际取 `DEEP_ROWS=15` | 注释补一句深度版行数 | safe |
| insight.rs:236 | `Some(0.1)` 温度魔法数（同 compound.rs:130 / review.rs:38） | 共享常量（见 compound 条） | safe |

## exemplar.rs（20 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| exemplar.rs:29 | `fewshot` `unwrap_or_default()` 吞 PG 错误且无日志，语料静默缺席 | 失败时 `tracing::warn!` | safe |
| exemplar.rs:50 | `live_warehouse_tables` 吞错→空集→DMS 全部语料被过滤光（静默 fail-closed） | 失败时 `warn!`（含 ds） | safe |
| exemplar.rs:157 | `suggest_questions` 同款吞错无日志 | `warn!` | safe |
| exemplar.rs:295 | `nearest` 同款吞错：语义缓存静默 miss，排障零线索 | `warn!` | safe |
| exemplar.rs:30,158,296 | 一轮问答内 fewshot+suggest+nearest 各自重查一遍 `meta.table_doc`（同 ds 同数据最多 3 次往返） | 每轮缓存一次传入 | test |
| exemplar.rs:68-73,84-89 | 去空白+去反引号+lowercase 的 compact 变换与 mod.rs:354-359 共三份拷贝 | 抽共享 fn（如收进 kernel/本 mod） | safe |
| exemplar.rs:84-89 | 每个 metric 每次调用都重算 `expression()` 的 compact 串（`METRICS` 是静态表） | 预计算缓存（`LazyLock<Vec<String>>`） | safe |
| exemplar.rs:110-125 | `tables` Vec 先收集，`valid` 判定里又逐条重算 `parts.last()` | 复用 `tables` 迭代 | safe |
| exemplar.rs:182-197 | `save_with_context` `INSERT...WHERE NOT EXISTS` 无 `ON CONFLICT`：并发下孪生行（除非有唯一索引兜底） | 确认唯一约束或改 `ON CONFLICT DO NOTHING` | test |
| exemplar.rs:195-196 | `.map(rows_affected>0).unwrap_or(false)`：PG 错误被谎报成「已存在」，调用方据此跳过存向量 | 错误分支 `warn!` 后再 false | safe |
| exemplar.rs:200-210 | `set_embedding` 非法 `qvec` → `$1::vector` 解析错被 `let _` 吞（注释虽声明刻意吞错，定位时仍零线索） | `debug!` 级留痕 | safe |
| exemplar.rs:229-235 | `set_status` 不查 `rows_affected`，question 打错静默 no-op 仍 `Ok` | `rows_affected()==0` 时 `warn!` | safe |
| exemplar.rs:246 | `set_ai_review` 非 `"negative"` 一律按 pending 处理（`"negativ"` 之类 typo 静默归 positive 侧） | 入参白名单校验（positive/negative） | test |
| exemplar.rs:266-273 | `pending` `LIMIT $1` 未夹紧，负 limit PG 直接报错 | `limit.max(0)` | safe |
| exemplar.rs:331-336 | `candidate_lessons` 同款未夹紧 | 同上 | safe |
| exemplar.rs:309-322 | `save_lesson_candidate` `NOT EXISTS` 同款竞态 + `unwrap_or(false)` 吞错 | 同 182-197 修法 | test |
| exemplar.rs:340-347 | `set_lesson_status` 不查 `rows_affected` | 0 行时 `warn!` | safe |
| exemplar.rs:349-403 | `#[cfg(test)] mod tests` 位于文件中段，405 行后还有 pub 函数，与全仓「测试在文件尾」惯例不符 | 移到文件尾 | safe |
| exemplar.rs:410-411,428-429 | `log_correction`/`log_failure` 非 traced 包装全仓零调用（caller 全走 `_traced` 版，见 run.rs:746/801/1212） | 删除或标注仅供兼容 | safe |
| exemplar.rs:22 | `ORDER BY word_similarity($1, question)` 无 `%` 阈值预筛，enabled 语料全表逐行算相似度，语料涨后是全扫 | 加 `question % $1` trgm 索引预筛（注意阈值语义变化） | test |

## embed.rs（20 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| embed.rs:62 | `reqwest::Client::builder().build().expect(...)` 构建失败直接 panic，与「服务挂时静默降级」的整体气质不一致，且未注释说明启动即崩是刻意取舍 | 注释说明，或 `new` 返 `Result` | safe |
| embed.rs:53 | `now()` 的 `unwrap_or(0)`：时钟异常时 `now() < cooldown_until` 恒真 → 永久冷却（fail-closed），此取舍无注释（doc.rs:212 同款） | 加一行注释钉住 fail-closed 语义，或改 `unwrap_or(u64::MAX)` fail-open | safe |
| embed.rs:84 | `resp.json().await.ok()?` 不看 HTTP 状态码：服务持续 500 时永不进熔断（与 send 失败不对称），每个问句白付一次 HTTP | `resp.error_for_status()` 区分 4xx/5xx，5xx 计入熔断 | test |
| embed.rs:84 | json 解析错误被 `.ok()?` 吞掉、零留痕；crate 内 mysql.rs/fixed.rs 都有 warn/debug 先例，tracing 已是 connector 依赖（Cargo.toml:14） | 加 `tracing::debug!(err=%e, "embed 响应解析失败，降级")` | safe |
| embed.rs:91-94 | `Err(_)` 丢弃 reqwest 错误：熔断 300s 这一重大状态切换无任何日志，排障只能猜 | `Err(e)` + `tracing::warn!(err=%e, "embed 服务不可达，熔断 {COOLDOWN_SECS}s")` | safe |
| embed.rs:89+171 | 条数校验过了但行维度不校验：`parse_embeddings` 的 `filter_map` 静默丢非数值元素，`[1.0,"x"]` 变 `[1.0]` 照样通过 `m.len()==texts.len()` → 短维度向量写进 pgvector | 校验所有行等长且非空，不符整批 None | test |
| embed.rs:171 | `as f32` 对超范围 f64（如 1e300）静默变 inf → pgvector 拒收时错误指向 SQL 层而非数据来源 | 校验 `is_finite()` 或与上行维度校验合并 | test |
| embed.rs:103,109 | `query_memo.lock().unwrap()` 对 Mutex 中毒裸 panic | `unwrap_or_else(\ | e\ |
| embed.rs:103-109 | 并发 miss：两个并发 `embed_query("同文")` 都未命中 → 重复发 HTTP（last-writer-wins，无害但无注释） | 一行注释说明「并发重复无害、不加 in-flight 锁」 | safe |
| embed.rs:123-132 vs 137-146 | `embed_passages`/`embed_queries` 逐字重复，仅 mode 不同 | 抽私有 `embed_batched(&self, texts, mode)`，两 pub 函数各一行 | safe |
| embed.rs:154 | 🔴 潜在 bug：Query 模式超时恒 3s 与条数无关，但 `embed_queries` 的调用方是后台批任务（server/src/embed_fill.rs:87，一批 64 条）；按本文件 18-20 行实测口径 64 块≈2.2s，负载高时必超 3s → 超时 → 熔断 300s——正是 115-122 行注释修掉的那族问题在 query 侧的复刻 | `embed_queries` 走 passage 式按条数预算（或独立常量），用户侧 `embed_query` 保持 3s | test |
| embed.rs:92-93 | 熔断槽 query/passage 共享：语料侧一次批超时把问句侧也熔断 300s（一次后台重建失败影响在线问答 5 分钟） | 拆两个 `cooldown_until`（按 mode），或注释说明共享是刻意 | test |
| embed.rs:156 | `TIMEOUT_SECS * 1000 + PASSAGE_MS_PER_TEXT * n as u64`：秒常量×1000 与毫秒常量相加易读错，`as` 优先级靠默认 | `Duration::from_secs(TIMEOUT_SECS) + Duration::from_millis(PASSAGE_MS_PER_TEXT * n as u64)` | safe |
| embed.rs:161-163 | `build_body` 用 `json!` 宏对 64×640 字全量 clone 成 `Value` 再序列化，两道分配 | 改 `#[derive(Serialize)] struct Body<'a>{texts:&'a [String], query:bool}` 零拷贝（wire 形状 `{"texts":[...],"query":bool}` 逐字不变，有 195-200 行测试守） | safe |
| embed.rs:178-188 | `to_pgvector` 每维一次 `format!` 分配（512 维=512 次小分配），且无容量预估 | `String::with_capacity(v.len()*9+2)` + `use std::fmt::Write; write!(&mut s, "{x:.6}")`（输出逐字不变，213 行金标守） | safe |
| embed.rs:184 | NaN/inf 输入格式化成 `NaN`/`inf` 字面量，pgvector 解析报错且文案指向 SQL 而非向量来源 | `x.is_finite()` 防御或注释说明信任服务端 | safe |
| embed.rs:49-54 | `now()` 与 doc.rs:208-213 逐字重复 | 抽 `pub(crate) fn now()` 到 connector 内公共处 | safe |
| embed.rs:100 | 文档注释「512 维」硬编码维度数，实际由服务端模型决定，换模型即过期 | 删维度数或改「维度由服务端模型定」 | safe |
| embed.rs:285 | 测试桩 `content_len` 缺 Content-Length 时 `unwrap_or(0)` → 提前 break、空 body 喂 serde_json → 295 行桩内 panic，测试挂在客户端超时而非清晰失败 | 桩内缺头时 `expect("stub 需要 Content-Length")` | safe |
| embed.rs:295-296 | 桩内两处 `unwrap()` panic 在 spawned task 里，表现为客户端超时类假错 | 改 `expect` 带「桩收到坏请求」文案 | safe |

## docs/CONFIG.md（19 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| docs/CONFIG.md:10 | 引用报文尾部 "`insight_enabled`" 已过期：`Settings` 后续新增 `insecure_login_fallback`、`kb_manager_grants`（db.rs:204-229），serde 按声明顺序列键，真实报文尾部应是 `kb_manager_grants` | 尾部改成省略号或更新为实际尾键 | safe |
| docs/CONFIG.md:19,53,162,212 | `D1`/`AX73`/`D10`/`Y3` 等内部裁决编号散落全文却无对照索引，新读者无法回查出处 | 文末加一行编号出处说明（或删掉编号） | safe |
| docs/CONFIG.md:47 | `mysql_url`「不填的后果=身份认证与权限计算不可用」不实：该键无 `#[serde(default)]`（db.rs:78），缺键=启动 missing field 硬失败，与同表 `pg_url` 行「起不来」口径自相矛盾 | 改为「缺键启动即失败」 | safe |
| docs/CONFIG.md:56-57 vs 78-80 | 同一条生产库红线两处列举不一致：前者含 LIKE/schema 探针、后者含 UNION/子查询/无界排序，运维不知以哪份为准 | 合并成一份清单，另一处改为引用 | safe |
| docs/CONFIG.md:59 | `fallback_db_target` 标识符全仓零命中；且「删除当前目标时会先切到其他目标」与实现相反——settings_api.rs:1214-1232 测试钉死：删当前目标直接 409、「必须先切走再删」、删除端点不得隐式切换（`assert!(!body.contains("persist_db_target"))`） | 改为「删除当前生效目标被 409 拒绝，需先手动切换」并去掉虚构标识符 | safe |
| docs/CONFIG.md:113 | 只写 `qwen`/`deepseek`，自定义供应商机制 `llm_providers`（db.rs:128-131、settings.example.json:28-36）全文未文档化 | 补 `llm_providers` 小节 | safe |
| docs/CONFIG.md:116 | 「不填的后果：与目录默认合并（目录补缺省字段）」表述绕，两行才说清 | 简化为「缺省用目录默认」 | safe |
| docs/CONFIG.md:117 | 「目录默认（qwen3.7-flash / deepseek-chat）」过时：DeepSeek 目录默认实为 `deepseek-v4-flash`/`deepseek-v4-pro`（db.rs:564-565），`deepseek-chat` 已不存在 | 更新型号名 | safe |
| docs/CONFIG.md:127-128 | capabilities 响应字段写成 `vision_fallback`，实际键名是 `fallback_vision_provider`（main.rs:1787），照文档解析响应会拿不到 | 改键名 | safe |
| docs/CONFIG.md:152-160 | 未说明 `mcp_keys` 改动须重启才生效（db.rs:172「轮换＝改配置重启」），而紧邻的 :120 刚讲 LLM「运行时切换不需要重启」，读者易类推错 | 明示「mcp_keys 无热更」 | safe |
| docs/CONFIG.md:201 | 缓存段漏掉 60s **负缓存**（上游判失效的 token 同样缓存 60s，xcx_api.rs:40/74/178）——安全相关语义，运维排障需要知道 | 补一句负缓存说明 | safe |
| docs/CONFIG.md:211 | 「axum 默认 2MB 会先触发」语义反了/含混：body limit 已显式设为 `kb_max_mb`，默认不会触发；原意是「若不显式设置就会被 2MB 默认截断」 | 重写括号内说明 | safe |
| docs/CONFIG.md:212 | ⚠️ 例外段（「/api/ask 主链暂用默认值、Y3 包未接线」）已过期：knowledge.rs:35-36 注释明写「主链与 kb_api / kb_eval / mcp 四条链至此全部吃页面可配的生效值」 | 删除例外段 | safe |
| docs/CONFIG.md:217 | 「生产只接受 HTTPS」漏了代码内置例外：`http://localhost` 也被放行（wework.rs:65） | 补 localhost 例外 | safe |
| docs/CONFIG.md:221 | `GET /health` 未指明这是 **Python 解析/向量服务**（embed_service.py:13，:8077）的端点，极易与 Rust `/api/health`（main.rs:1239，:8100）混淆 | 补服务名与端口 | safe |
| docs/CONFIG.md:229-230,238-241 | 「.docx/.pptx 装了但本机不可用（lxml 被 SAC 拦，`DLL load failed ... etree`）」疑似过期：当前 .venv 为 lxml 6.1.1，`from lxml import etree` 与 `import docx, pptx` 均成功 | 复跑 `parse_ok`/端到端解析验证后更新「现状」列与下文说明 | test |
| docs/CONFIG.md:246 | 挂载示例 `-v ./settings.json:/app/settings.json` 与 scripts/serve.ps1:52 实际挂载 `settings.docker.json:/app/settings.json` 不一致（run.ps1:8-13 也是 docker 优先） | 统一示例文件名或注明两者皆可及优先级 | safe |
| docs/CONFIG.md:248 | `dev_token` 在代码中已不存在（crates 全仓零命中；现行为 `insecure_login_fallback`，db.rs:205-222）——照此写配置会触发 `deny_unknown_fields` 启动失败；CODE-REVIEW-2026-07-30.md:144 早已指出仍未修 | 改写为 `insecure_login_fallback` 条目（默认 false、开启 warn 留痕） | safe |
| docs/CONFIG.md:205-217（其它表） | 全文缺 `insecure_login_fallback` 条目，而 DEPLOY.md:77 与 README.md:41 都引用它——配置主文档反而查不到这个安全开关 | 补条目 | safe |

## xcx_api.rs（19 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| xcx_api.rs:5 | 文件头称「模块声明已带 `#[allow(dead_code)]`，接线时一并删掉它」—— main.rs:36 `mod xcx_api;` 本无该属性且路由 1404-1405 已接线，头部注释过期 | 更新头部为「已在 main.rs 接线」 | safe |
| xcx_api.rs:43-44 | rustdoc 列表 44 行只缩进 1 空格，渲染出来不在列表项内 | 对齐缩进 | safe |
| xcx_api.rs:84-85,349-355 | 限长常量 500/2000 与报错文案「最多 500 字」「最多 2000 字」两处硬编码，改常量不改文案即失真（钉值测试只钉常量不钉文案数字） | 文案用 `format!` 拼常量，或测试断言文案含常量值 | safe |
| xcx_api.rs:255,267,274 | `.expect("xcx 缓存锁中毒")`：一次 put 期间 panic 即永久中毒，此后所有 xcx 请求 500 | `unwrap_or_else(PoisonError::into_inner)` 自愈 | test |
| xcx_api.rs:263-279 | 缓存 miss 无 single-flight：冷启动/缓存集中过期时 N 个并发同 token 请求各打一次上游（放大恰好落在「缓存要防的事」上） | in-flight map 去重或注释声明接受 | test |
| xcx_api.rs:312 | 上游业务码非 0 一律 warn：常规 token 过期（30007）是用户级日常事件，每 token 每 60s 一条 warn 属噪音 | 30007/30012 降为 debug/info，未知码保留 warn | safe |
| xcx_api.rs:320 | `body.get("data").cloned()` 克隆整棵 data 子树（员工信息 payload 可能不小），而 `parse_identity` 只需要 `&Value` | `body.get("data").unwrap_or(&Value::Null)` 传引用 | safe |
| xcx_api.rs:373 | `st.cfg()` 整表克隆 Settings（main.rs:130-132 `read().clone()`）只为取 `xcx_auth_base` 一个字段，且发生在每个 ask/me 请求 | AppState 加目标字段读取器或读锁内拷贝单字段 | safe |
| xcx_api.rs:375-379 | x-access-token 无长度上限即作缓存 key：hyper 头上限 ~8KB，1000 条 CAP × 8KB key ≈ 8MB，且超长 key 拖慢哈希；auth.rs:16 对同一上游 token 有 MAX_UPSTREAM_TOKEN_LEN=4096，本路径没有等价闸 | 超长按 token 失效拒（或截断前提示） | test |
| xcx_api.rs:417-420 | `prev_question` 先 trim 再限长，`prev_sql` 不 trim 直接限长：同为 2001 字符的空白填充，前者放行后者 400，口径不对称 | 两者同样 trim 后测长（或都不 trim） | test |
| xcx_api.rs:457-459 | `prev_sql` 不过滤空串：`prev_question` 给了、`prev_sql` 给 `""` 时 `Some("")` 直入问答管道，与 prev_q 的 `filter(!is_empty)` 不对称 | `req.prev_sql.as_deref().map(str::trim).filter( | s |
| xcx_api.rs:462-464 | `inspect_err` 的 warn 里 `reason = "chat_context_load_failed"` 是静态串，真实错误 `e` 被丢弃 —— 注释自称「warn 留痕」却没留真因 | `tracing::warn!(conv_id, reason = %e, ...)` | safe |
| xcx_api.rs:493 | 403/422 分类靠 `e.to_string().contains("无权访问数据源")` 子串匹配：上游错误文案一改措辞，权限拒绝静默降级成 422 | 错误链改 typed kind（或至少 `starts_with`+钉文案测试） | test |
| xcx_api.rs:503,513 | `serde_json::to_value(...).unwrap_or_else( | _ | json!({}))`：序列化失败静默回 `code:0 + data:{}`，客户端拿到空白答案且服务端零日志 |
| xcx_api.rs:517-518 | `let _ = save_msg(...)` 两处完全吞错无日志；同文件 456 行注释刚批判过「静默丢上下文查不出来」，丢历史同理 | 失败时 `tracing::debug!/warn!` 一行 | safe |
| xcx_api.rs:522-525 | payload 非 object 时 conv_id 静默不注入，客户端从此串不起多轮，无任何痕迹 | else 分支 debug 日志或包一层 object 兜底 | safe |
| xcx_api.rs:125-130 | 登录名嵌套回退查 `user/employee/sysUser/userInfo`，但 role/name 只查顶层：上游把完整身份塞进 `user` 对象时 role_code 丢失 → 多角色账号被 load_principal 拒成 403，与注释「上游各端返回结构不一」的防御初衷不自洽 | role/name 同样回退嵌套对象（或注释声明只认顶层角色） | test |
| xcx_api.rs:305-308 | `resp.json()` 对上游响应体无大小上限：上游被攻破/配置错误回巨型 body 时解析阶段吃内存 | `Content-Length` 预检或流式限长读取 | test |
| xcx_api.rs:66,68 vs auth.rs:509 | `x-access-token` 头名两处各写字面量（xcx 有 TOKEN_HEADER 常量，auth.rs verify_dms_token 手写字符串），改协议时易只改一边 | 共享常量（auth 导出或下沉公共模块） | safe |

## web/src/format.ts（18 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| web/src/format.ts:2,4,38,49,82 | 同样混入孤 `\r` 行尾 | 统一 LF | safe |
| web/src/format.ts:22 | 先 `replace(/,/g,'')` 再校验：`"1,2,3"`、`",,5"` 这类畸形串被静默解析成 123/5，掩盖脏数据 | 先按千分位格式校验再允许去逗号，或只去合法分组的逗号（带测试） | test |
| web/src/format.ts:32 | `(?:^ | _)id$` 与后面的 `ID$/i` 在 `/i` 下完全冗余（后者已覆盖前者） | 删 `(?:^ |
| web/src/format.ts:33 | 裸「率」字过宽：「汇率」「频率」「功率」全被判 percent，6.45 显示成「6.5%」是错数 | 收窄为「（毛利/利/税/费/占比/增长/…）率」白名单或词尾「率」+排除词，带测试 | test |
| web/src/format.ts:33 | 「同比/环比」直接判 percent：「同比增长额」「环比增量」是金额/单量，会被加 % 误显 | 同上，与「额/量」词尾联合判定 | test |
| web/src/format.ts:34 | money 词表漏「营收」（只匹配「收入」）：「营收」列落入 none→count 兜底，无 ¥；「售价/现价/金额」里「售价/现价」也漏 | 补 `营收\ | 售价\ |
| web/src/format.ts:35 | `库存(?:数\ | 量)` 要求后缀，裸「库存」列判 none 不压缩，与「库存量」显示不一致 | 允许裸「库存」，带测试 |
| web/src/format.ts:54 | `PROVINCE[String(v)]` 同一表达式算两遍 | `const name = PROVINCE[String(v)]; if (name) return name` | safe |
| web/src/format.ts:60 | percent 只做 `round(n,1)%` 不 ×100，×100 合同全靠三个调用点各自记（见 BiChart:93 / ResultPanel:283）——新调用方忘乘就静默错 100 倍 | 在 fmt 的 JSDoc 写明「percent 输入必须已是 0-100」；或集中 ×100 进 fmt | test |
| web/src/format.ts:61 | 负金额输出 `¥-1.23万`，与财务惯例 `-¥1.23万` 不一致；且与 grouping 路径（L74，负号在前）风格不一 | 统一负号位置，带快照测试 | test |
| web/src/format.ts:67 vs 70 | 注释「全端统一**最多** 2 位小数」，但 `toFixed(2)` 是**恰好** 2 位：10000 →「1.00万」，与 <1万 的「9,999」（去尾零）风格不一致 | 二选一：改注释为「恰好 2 位」(safe)，或 `.replace(/\.?0+万$/,'万')` 去尾零（test） | safe |
| web/src/format.ts:70 | `toFixed` 浮点边界：`1.005` 类值因二进制表示截断成「1.00」而非「1.01」 | 用 `round(n,2).toString()` 路径或注释声明可接受 | test |
| web/src/format.ts:74-81 | 每次调用 new 一个 `Intl.NumberFormat`：结果表格几百单元格 × 重渲染反复构造；同仓 ResultPanel.vue:270/293 已是模块级单例写法，两处不一致 | 提升为模块级常量（对齐 ResultPanel 写法） | safe |
| web/src/format.ts:75 | `0.0005` 消负零的 epsilon 是裸魔数，无注释说明防的是 `-0` 显示 | 加一行注释「防 -0」或提常量 | safe |
| web/src/format.ts:83-86 | `round()` 用 `n * p` 直接乘：`1.005*100=100.49999…` → Math.round 得 100，百分比显示偶发少 0.1 | 加 `Number.EPSILON` 修正，带测试 | test |
| web/src/format.ts:52 | 只挡 `''`，纯空白串 `'   '` 会走到 L58 原样输出空格占位 | `typeof v === 'string' && !v.trim()` 一并归空，带测试 | test |
| web/src/format.ts:57 | 四连 `\ | \ | ` 判非数值语义，可读性差 |
| web/src/format.ts:12 | Math.random 兜底分支无测试也无注释标注「非加密级」，会话 key 冲突概率无人盯 | 注释补一句「兜底非加密级，仅防撞 key」 | safe |

## UsagePanel.vue（18 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| UsagePanel.vue:21 | token 与 login 都缺时拼出 `?login_name=`（空值参数） | login 为空时不拼接参数 | safe |
| UsagePanel.vue:43 | days 未排序直接 `slice(-7)`，后端若非升序会取错区间且柱状图乱序 | slice 前 `days.sort((a,b) => a.date.localeCompare(b.date))` | safe |
| UsagePanel.vue:43+129 | 未去重 date，后端返回重复日期时 `:key="b.date"` 冲突 | normalize 里按 date 去重/聚合 | safe |
| UsagePanel.vue:68 | `${e}` 对非 Error 值输出「[object Object]」 | `e instanceof Error ? e.message : String(e)` | safe |
| UsagePanel.vue:83-84 | todayStr 用本地时区拼，后端 date 若按 UTC 生成，「今天」高亮会错一天；对话框跨午夜开着也会滞留 | 以后端返回的最大 date 为「今天」，或注释说明时区约定 | test |
| UsagePanel.vue:97-99+108 | 加载中请求未 AbortController，关闭弹窗后回包仍写 ref（结果白算） | onBeforeUnmount 里 abort | safe |
| UsagePanel.vue:115 | 关闭按钮只有 title，✕ 字符读屏不友好 | 加 `aria-label="关闭"` | safe |
| UsagePanel.vue:109 | 弹窗打开后焦点未移入对话框、无焦点回收 | 挂载后 focus 关闭按钮/首个可聚焦元素 | safe |
| UsagePanel.vue:117 | 加载态无 role=status，读屏感知不到 | 加 `role="status"` 或 aria-live | safe |
| UsagePanel.vue:118 | 错误态只有文案，无重试入口，只能关掉重开 | 加「重试」按钮调 load() | safe |
| UsagePanel.vue:121-123 | KPI 大数字无千分位，累计量大时难读 | `n.toLocaleString('zh-CN')` | safe |
| UsagePanel.vue:125 | 区块标题「近 7 天」与 L123 KPI 标签「近 7 天」重名，看不出一个是趋势图 | 改为「近 7 天趋势」 | safe |
| UsagePanel.vue:130-132 | 柱子无 hover 提示，0 值日也看不到日期+数值 | `<g>` 内加 `<title>{{ b.date }}：{{ b.count }} 次</title>` | safe |
| UsagePanel.vue:132 | `b.date.slice(5)` 假设 `YYYY-MM-DD` 格式，格式变了显示乱码 | normalize 校验 `/^\d{4}-\d{2}-\d{2}$/`，不符则原样显示 | safe |
| UsagePanel.vue:140+175 | 计数 `<b>` 定宽 36px，万级以上数字溢出 | 去掉定宽或 `min-width` + 允许换行/缩字号 | safe |
| UsagePanel.vue:147 | `<style>` 未 scoped，与 TracePanel.vue:180 的 scoped 写法不一（若无 teleport 需要） | 加 scoped 或注释说明刻意全局 | safe |
| UsagePanel.vue:151+163 | font-weight 750/650 在非可变字体上回退不一致 | 统一 700/600 或确认可变字体依赖 | safe |
| UsagePanel.vue:20-22 | authQuery 与 SkillsPanel.vue:39-41 逐字重复 | 抽到共享 util（如 api.ts） | safe |

## api/ai-chat/index.js（18 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| api/ai-chat/index.js:36 | 文案 `'您还未登录,请登录后查看该内容'` 用半角逗号，中文排版不规范 | 改全角「，」；tool/request/index.js:75 同款一并改 | safe |
| api/ai-chat/index.js:29 | tab 页白名单数组与 tool/request/index.js:70 两处手工维护，新增 tab 页易漏改一边 | 抽共享常量（如 tool/authPages.js）两处引用 | safe |
| api/ai-chat/index.js:21 | 模块 import 时即 `useLoginStatus(pinia)`，import 顺序变化时 store 可能未激活 | 延迟到 `handleAuthInvalid` 首次调用时取 | safe |
| api/ai-chat/index.js:52 | `rawRequest()` 允许无参调用，`path` 为 undefined 时 URL 拼成 `...undefined` 静默发请求 | 入口 `if (!path) return Promise.reject(new Error('path required'))` | safe |
| api/ai-chat/index.js:62 | 未登录也发 `'x-access-token': ''` 空头，后端可能把空串当"无效 token"而非"未带 token" | 无 token 时省略该 header | test |
| api/ai-chat/index.js:65 | 返回体是字符串（网关 HTML 错误页）时 `body.code` 为 undefined 且 statusCode 200 → 落到 L79 `resolve({code: undefined, msg: undefined})`，错误信息全丢 | `typeof body !== 'object'` 时 reject 新 Error | test |
| api/ai-chat/index.js:75 | 仅当 `body.code === undefined` 才把非 200 视为错误；代理返回 502+JSON `{code:1}` 会被当业务包 resolve，与 tool/request/index.js:47 的 502 特判口径不一 | 非 2xx 一律走错误分支 | test |
| api/ai-chat/index.js:76 | 文案 `服务器错误(${res.statusCode})` 半角括号，与 L90/93 全角文案风格不一 | 统一全角或统一格式 | safe |
| api/ai-chat/index.js:72,85,90,93 | reject 三种形态（`{authInvalid}` 对象、`{aborted}` 对象、Error），头部注释 L15-16 只文档化两种 | 注释补 `{aborted}` 形态约定，或统一错误对象 | safe |
| api/ai-chat/index.js:83 | 注释 `reject  aborted 标记` 双空格 typo | 改单空格 | safe |
| api/ai-chat/index.js:81-93 | fail 分支无任何 console 留痕，线上弱网/超时无法回溯原始 errMsg | reject 前 `console.warn('[ai-chat]', err)` | safe |
| api/ai-chat/index.js:100-106 | `aiAsk` 不校验 `question` 空串/纯空白，空调用浪费一次 60s 往返 | 入口 trim 后为空则直接 reject | test |
| api/ai-chat/index.js:104 | `conv_id ? {conv_id} : {}` 会把 falsy 值（0/''）丢弃，若后端 id 为数值 0 会静默丢上下文 | 改 `conv_id != null ? {conv_id} : {}` | test |
| api/ai-chat/index.js:23 + tool/request/index.js:16 | `isShowingLoginModal` 两处各一把锁，AI 请求与主站请求同时 401 时会先后弹两个登录 modal | 锁提升到共享模块 | test |
| api/ai-chat/index.js:10 | 头部注释写缺省 `http://117.72.32.186`，但未同步 config/index.js:11 的"微信生产必须 HTTPS+合法域名"警告 | 注释补一句指向 config 警告 | safe |
| api/ai-chat/index.js:15 | 注释用魔法数 `code===0`，文件已引 `RequestCodeEnum.SUCCESS` | 注释改写 `RequestCodeEnum.SUCCESS` | safe |
| api/ai-chat/index.js:59 | AI 问答复用全局 60s timeout，长答复场景无说明 | 注释说明取舍或允许 aiAsk 传自定义 timeout | safe |
| api/ai-chat/index.js:37-43 | modal 用户点取消后无任何反馈/引导，停留在无权限页面 | cancel 分支给轻提示（如 toast「可在我的页登录」） | test |

## kg_api.rs（18 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| kg_api.rs:104-106 | `load_principal` 失败（含 MySQL 宕机）一律 403「身份或角色不可用」且底层错误被 `map_err(\ | _\ | )` 吞掉无日志，DB 故障会被误判成身份问题 |
| kg_api.rs:137-142 | `migrate` 被 build(:207)/status(:316)/failed_chunks(:446)/reset(:517)/reconcile(:573) 每请求调用，每次 2 条 DDL RTT；status 是构建期轮询热点 | 用 per-pool `OnceCell`/AtomicBool 进程内只跑一次，失败再重试 | safe |
| kg_api.rs:202,214,311,326,360,373,394,412,441,453,471,475,512,525,531,567,586,591,595,599 | 全文件 20+ 处 `map_err(\ | _\ | err(500,...))` 吞掉 sqlx 错误原文，线上 500 无任何日志可查 |
| kg_api.rs:220-225 | spawn 的任务里 `kg::build_space` 若 panic，`finish` 不会执行，状态行卡 `building` 直到 30 分钟过期才被接管 | spawn 内用 `catch_unwind`/兜底闭包保证 panic 也落 failed 终态 | test |
| kg_api.rs:241-243 | `o.total/done/failed as i32`：usize→i32 溢出静默回绕 | `i32::try_from(..).unwrap_or(i32::MAX)` | safe |
| kg_api.rs:234-235 | `to_value(&o.failed_samples)` 失败静默回退 `[]`，失败样本无声丢失 | `unwrap_or_else` 分支加 `tracing::warn!` | safe |
| kg_api.rs:281-282 | `PgProgress::report` 同样的 `to_value` 静默回退 `[]` | 同上，加 warn | safe |
| kg_api.rs:329-336 | status 逐列 `try_get(...).unwrap_or*`：列类型漂移时 state 透出 `""`（不是契约里的 idle/building/done/failed 四值）且无日志 | state 列失配时 warn 并回退 `"idle"` 或直接 500 | test |
| kg_api.rs:361 | center 传了但 trim 后为空 → 静默降级为全量 TOP 子图，调用方无法区分「center 无效」与「没传 center」 | 空白 center 显式 400，或响应里标记 `center_ignored:true`（后者动 wire 形状，择一） | test |
| kg_api.rs:375-380 | nodes/edges 逐元素 `json!` + `collect`，无预分配 | `Vec::with_capacity(sg.nodes.len())` 后 push | safe |
| kg_api.rs:462-468 | `sample_map` 用 `(String, i64)` key，:487 每次查询 `doc_id.clone()` 纯为查表克隆 | key 改 `(&str, i64)` 借用 samples 内的 String | safe |
| kg_api.rs:520-526 + 528 | reset 的 building 检查与 `clear_space`/`DELETE` 之间是 TOCTOU：检查通过后另一请求可认领到 building，reset 继续清图并无谓词 `DELETE` 状态行，把正在跑的构建状态行删掉（status 丢跟踪） | DELETE 加 `AND state<>'building'` 谓词，`rows_affected=0` 则 409 | test |
| kg_api.rs:576-578 | reconcile 同款 TOCTOU（检查后、DELETE 前可被 build 认领） | 与 reset 同思路：先抢占式占行或用单条条件语句收口 | test |
| kg_api.rs:533,627 | reset/reconcile 的 info 日志没有操作者 login，运维无法追溯谁清的图 | 日志加 `operator = %v.login` 字段 | safe |
| kg_api.rs:627-632 | reconcile 已执行日志漏 `relations` 删除数（:600 起算了 `relations` 且进了响应，独漏日志） | info 宏加 `relations = relations` | safe |
| kg_api.rs:593-599 | `dangling_entities` 与 `relation_count_of_chunks` 互不依赖却串行 await，白等一个 RTT | `tokio::join!` 并发两条只读查询 | safe |
| kg_api.rs:613-621 | 三步 DELETE 非事务：中途失败留下「边删了块还在」半态，且 500 响应丢掉已删步数信息 | 三步包一个事务，或每步失败时 warn 记录已完成进度 | test |
| kg_api.rs:199,219 | `space_param(...)?.to_string()` 复制一份后 :219 又 `.clone()` 给 spawn；BuildReq 本就持有 String | 原地校验后 move，只 clone 一次给 spawn | safe |

## lex.rs（18 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| lex.rs:12 | `chars().collect::<Vec<char>>()` 4 倍字节内存；引号/注释符全 ASCII，可按字节扫 | 改 `as_bytes()` 状态机（行为需逐测试钉住） | test |
| lex.rs:20-22 | `\` 转义对所有方言生效；PG `standard_conforming_strings=on` 下 `'\'` 是完整字面量，此处会把后续文本吞进字符串态 → 扫描漏词（AST 主防线在，属二防漏判） | 注释声明该偏差；或按方言传 flag | test |
| lex.rs:41-45 | `#` 当行注释：PG 里 `#` 是位运算 XOR，`a # b` 之后的文本被吞 → 二防漏判 | 同上，注释声明或方言化 | test |
| lex.rs:36-40 | `--` 不要求后随空白（MySQL 需要 `-- `）：`1--2` 被当注释；方向安全但与 MySQL 词法不同，注释未写 | 注释一句「从宽处理」 | safe |
| lex.rs:82 | `chars[i..i+5].iter().collect::<String>()` 每个字符位置都分配 String 只为比 5 字符 | 直接逐字符 `eq_ignore_ascii_case` 比较 | safe |
| lex.rs:84,93,96 | 84 行无非空判断直接 push（可入空串），靠 96 行兜底过滤；93 行却判断——两处风格不一 | 84 行同样加非空判断，删 96 行过滤 | safe |
| lex.rs:100-108 | `first_ident_of("1 = 1")` 返回 `Some("1")`——数字被当标识符（消费者拿它查列名必落空，语义靠下游兜） | 首字符非字母/`_`/反引号时返回 None | test |
| lex.rs:106 | 只剥反引号不剥双引号，与「方言双引号标识符」不一致（PG 形态的 cond 会带引号残留） | `trim_matches( | c |
| lex.rs:133 | `t.starts_with("t_")` 把 DMS 表前缀硬编码进宣称「零 DMS 语料」的 kernel | 前缀改为参数传入，或在头注声明此函数是 DMS 形态专用 | safe |
| lex.rs:135 | 别名排除词只有 on/join：`FROM t_x WHERE ...` 会把 `WHERE` 当别名收下（main.rs:399 传的是整段 SQL，非固定形态） | 排除集补 where/group/order/limit/on/using/left/right/inner/full/cross/join/逗号 | test |
| lex.rs:128-144 | 逗号连接 `FROM t_a a, t_b b`：t_b 前驱是 `a,` 不命中 join/from → 第二表静默漏 | 前驱 token trim 逗号后再判，或显式处理逗号 | test |
| lex.rs:111 | 注释「组合器自己拼的串形态固定」与消费者现状不符（main.rs:399、admin_api.rs:373 传任意 SQL） | 注释更新为消费者清单 | safe |
| lex.rs:150-153 | pattern 先 `format!` 再 `to_lowercase` 两次分配 | `format!("{}.", alias.to_lowercase())` 一次成型 | safe |
| lex.rs:179-185 | KEYWORDS 缺 `SEPARATOR`（`GROUP_CONCAT(... SEPARATOR ',')` 会被改成 `a.SEPARATOR` 直接产出坏 SQL）、`XOR`、`REGEXP`、`RLIKE`、`DIV`、`MOD` | 补齐关键词（每加一个都是行为变化，带测试） | test |
| lex.rs:196 | `KEYWORDS.contains(&up.as_str())` 每 token 线性扫 40 词 | 排序 + binary_search 或 phf/matches | safe |
| lex.rs:197 | `push_str(&format!("{alias}.{tok}"))` 多一次临时分配 | 两次 `push_str` | safe |
| lex.rs:224 | token 字符不含反引号：`` `col` `` 会被切成 ` + col + `，col 照常加前缀 → 产出 `` `a.col` ``（单个标识符语义全变） | 反引号段当引号段处理（类似 in_quote 状态） | test |
| lex.rs:204-217 | 只护单引号字面量；双引号串（MySQL 默认字符串）内标识符会被加前缀 | 增加 `in_dquote` 对称分支 | test |

## docker/web/nginx.conf（17 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| docker/web/nginx.conf:2 | 无 `listen [::]:80;`,IPv6-only 客户端直接连接失败 | 补一行 IPv6 listen | safe |
| docker/web/nginx.conf:1-3 | 无 `server_tokens off`，错误页/Server 头泄露 nginx 精确版本号 | server 段加 `server_tokens off;` | safe |
| docker/web/nginx.conf:4-5 | 注释只说「20MB > 默认 1m」，没解释为何定 25m 而非 20m：后端产品口径是单文件 ≤20MB(crates/server/src/db.rs:381),20–25MB 区间的请求过 nginx 后被后端 JSON 拒绝——行为正确但注释缺「余量给 multipart 开销/与后端 20MB 对齐」这句，后人容易改成 20m 反而制造边界困惑 | 注释补一句 25m 与后端 20MB 口径的关系 | safe |
| docker/web/nginx.conf:8-11 | gzip 缺 `gzip_vary on`：响应无 `Vary: Accept-Encoding`，共享缓存可能把压缩副本发给不支持 gzip 的客户端 | 加 `gzip_vary on;` | safe |
| docker/web/nginx.conf:7-11 | 缺 `gzip_proxied`:L7 注释承诺「/api JSON 压缩」，但默认 `gzip_proxied off`——一旦站点置于 CDN/上层代理之后（请求带 Via),/api 压缩静默失效，注释承诺不再成立 | 加 `gzip_proxied any;` | safe |
| docker/web/nginx.conf:16 | `location /api/` 不匹配裸 `/api`（无尾斜杠）及拼错的类 API 路径：它们落进 SPA 兜底返回 200 text/html，调试时把 HTML 当 API 响应，极其迷惑 | 加 `location = /api { return 404; }`（或 308 到 `/api/`) | safe |
| docker/web/nginx.conf:17 | `host.docker.internal` 在 nginx **启动期**解析：Docker Desktop 未运行或 Linux 裸机（无 `--add-host`）时 nginx 直接 emerg 拒启，全站宕而非仅 /api 挂；配置里无一字说明此前提 | 至少加注释「仅 Docker Desktop;Linux 需 --add-host 或改 IP」；根治用 `resolver 127.0.0.11` + 变量延迟解析 | test |
| docker/web/nginx.conf:17 | 无 upstream keepalive：每个 /api 请求都对 8100 新建 TCP 连接，问数高峰期无谓握手开销；`proxy_http_version 1.1` 已设，缺的是 keepalive 配套 | 加 `upstream dms_api { server host.docker.internal:8100; keepalive 32; }` + `proxy_set_header Connection "";` | test |
| docker/web/nginx.conf:23-24 | 300s 超时与后端/文档侧的 300 秒熔断约定（serve.ps1:24 注释）是同一个数，但此处无注释链接；任一侧改值另一侧静默漂移 | 注释交叉引用「与后端熔断 300s 对齐，改值需同步」 | safe |
| docker/web/nginx.conf:17 | 无 `proxy_connect_timeout`：后端宕机时默认等 60s 才返回 502，用户以为是慢查询 | 加 `proxy_connect_timeout 5s;` 快速失败 | test |
| docker/web/nginx.conf:27-29 | 无静态资源缓存策略：vite 产物带 content hash(`/assets/index-BZRDcJKJ.js`)，却无 immutable 长缓存，每次访问对 ~1.1MB 资产逐个发条件请求重验证 | 加 `location /assets/ { expires 1y; add_header Cache-Control "public, immutable"; }` | safe |
| docker/web/nginx.conf:27-29 | index.html 无 `Cache-Control: no-cache`：浏览器启发式缓存可能让旧 HTML 引用已删除的 hash 资产 → 发版后白屏（与 index.html:9 白屏问题互为因果链） | 加 `location = /index.html { add_header Cache-Control "no-cache"; }` | safe |
| docker/web/nginx.conf:1-30 | 全文无 `X-Content-Type-Options: nosniff`，尤其 /api JSON 与上传文件预览存在 MIME 嗅探面 | server 段加 `add_header X-Content-Type-Options nosniff;` | safe |
| docker/web/nginx.conf:1-30 | 无 `X-Frame-Options`/CSP `frame-ancestors`——注意 **不能直接加 DENY**:integrations/dms-home/index.vue:3 用 iframe 嵌本应用，加错会破坏集成 | 如需加固，用 `frame-ancestors` 白名单列出 dms-home 源 | test |
| docker/web/nginx.conf:16-25 | 后端宕机时用户看到 nginx 默认英文 502 页，/api 调用方收到 HTML 而非 JSON，前端错误处理走错分支 | `error_page 502 503 504` 自定义友好页；/api 下返回 JSON 体 | safe |
| docker/web/nginx.conf:13 | `root /usr/share/nginx/html;` 无注释说明文件来源：docker/web 下没有 Dockerfile，产物靠挂载 web/dist 进容器，新人不知道 html 从哪来 | 加注释「挂载 web/dist → 此路径」 | safe |
| docker/web/nginx.conf:1-30 | 无含 `$request_time`/`$upstream_response_time` 的 log_format：慢问数排查（区分 nginx 排队 vs 后端耗时）缺基础数据 | 定义 log_format 并在 /api access_log 启用 | safe |

## kb_mindmap_api.rs（17 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| kb_mindmap_api.rs:10-11 vs 480 | 文件头注释说 sections 端点「刻意不注册 main.rs」，:480 注释说「已在 main.rs 接线」——两份注释直接矛盾 | 核实 main.rs 实际注册状态，统一两处注释 | safe |
| kb_mindmap_api.rs:680-683 | 注释承诺「接线后 `#[allow(unused_imports)]` 一并删掉」，若 :480「已接线」属实则该 allow 是赖着不走的死豁免 | 接线属实则删 allow；未接线则改 :480 注释 | safe |
| kb_mindmap_api.rs:111-113 | `viewer` 的 `load_principal` 失败吞错无日志（同 kg_api:104 的家族） | map_err 内 warn 底层错误 | safe |
| kb_mindmap_api.rs:92,96-97 | `kb_err` 的 Upstream/Db 变体细节被完全丢弃且无 warn——5xx 排障零线索 | 5xx 两个分支先 `tracing::warn!(error=%e)` 再映固定文案 | safe |
| kb_mindmap_api.rs:199 | 文档名截断 `take(40)` 是裸魔数（同文件 MAX_NAMES_PER_BRANCH 等都有名） | 提常量 `MAX_PROMPT_NAME_CHARS: usize = 40` | safe |
| kb_mindmap_api.rs:202,204 | `push_str(&format!(...))` 每行一次临时 String 分配 | 改 `write!`/`writeln!` 直接写进 out | safe |
| kb_mindmap_api.rs:262 | `reply.content.as_deref()?`：content 缺失时静默 None；:253/:258 的传输失败与超时都有 warn，独这条路径没有 | 缺 content 分支补 `tracing::warn!` | safe |
| kb_mindmap_api.rs:301 | `to_string(body).unwrap_or_else( | _ | "{}".into())`：Value 序列化实际不可失败，fallback 语义是「把毒值写进缓存」的死代码 |
| kb_mindmap_api.rs:321,353 | space_id 不 trim、无长度闸；kb_eval_api.rs:277-283 的 `normalize_space` 有 trim+64 字符闸——同族端点口径不一 | 复用 normalize_space 同款校验 | test |
| kb_mindmap_api.rs:325-331 | 缓存读 DB 失败直接 500；缓存只是加速器，导图完全可降级为「按未命中重新生成」 | KV_GET 失败时 warn 后走 generate 路径 | test |
| kb_mindmap_api.rs:341,358 | `write_cache` 在响应路径上同步 await，白加一个 RTT；失败本来就只 warn | `tokio::spawn` 后台写缓存 | test |
| kb_mindmap_api.rs:409-415 | 偏移对 `e <= s`（退化跨度）落入 `_ => 全文保留` 分支，与「偏移缺失」同处理，但 :398-400 注释只声明了缺失情形 | 注释补一句退化跨度同按缺失处理 | safe |
| kb_mindmap_api.rs:525-527 | `clip_excerpt` 循环里每字符都 `out.chars().count()`，O(n²) 无谓重算（上界 160 仍是浪费） | 维护一个 `n: usize` 计数器替代重复 count | safe |
| kb_mindmap_api.rs:548,559 | `bucket_sections` 每块线性 `find` 桶；且桶数超 MAX_SECTIONS 后仍继续新建桶吃内存、最后才 truncate | 满 100 桶后只累计已有桶、不再新建（对截断后结果零影响）；可选 HashMap<&str,usize> 索引 | safe |
| kb_mindmap_api.rs:443-446 | doc_for_viewer 与 load_chunks 之间文档被删 → markdown 空 → 404 文案只提「解析中或解析失败」，把「刚被删」误述 | 404 文案补「或已被删除」 | safe |
| kb_mindmap_api.rs:460 | `let _row = ...` 只为过闸，可 `let _ =` 语义更直白 | 改 `let _ =` | safe |
| kb_mindmap_api.rs:463-470 | doc_chunks 逐块 `json!` collect 无预分配 | `Vec::with_capacity(chunks.len())` | safe |

## kb_eval_api.rs（17 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| kb_eval_api.rs:125-131 | `reap_interrupted` 不像 kg_api.rs:154-156 先 migrate 再收割——启动编排若把 reap 排在 migrate 前会直接报错 | 函数内先 `migrate(store).await?`（幂等），与 kg 对齐 | safe |
| kb_eval_api.rs:259-261 | `eval_viewer` 的 `load_principal` 失败吞错无日志（同族第三处） | map_err 内 warn | safe |
| kb_eval_api.rs:321 | spawn 的 `run_eval` 若 panic，run 行永远卡 `running`（收割只在启动时跑），且无日志 | spawn 内 catch_unwind 兜底 UPDATE failed | test |
| kb_eval_api.rs:485,498 | 终态 UPDATE 用 `.is_ok()` 丢弃错误细节，:502 的 warn 没有 `error = %e`；kg_api.rs:261 同款场景有——两文件日志口径不一 | 改 `if let Err(e) = ...` 并把 e 带进 warn | safe |
| kb_eval_api.rs:623-635 | `progress()` 同样 `is_err()` 丢细节，warn 无 error 字段 | 同上 | safe |
| kb_eval_api.rs:553,568 | `st.cfg()` 每题取两次（×sample_size 次），循环外可只取一次 | 循环前 `let rrf = st.cfg().kb_rrf_weights;`（若可 clone）hoist | safe |
| kb_eval_api.rs:539-593 | 逐题全串行（出题→检索→答案→评审），sample_size=100 时 run 时长 ≈ 300 次串行 LLM/检索 RTT | JoinSet + 局部 Semaphore 有界并发（2-4 路），permit 口径不变 | test |
| kb_eval_api.rs:541,593 | 每题一次 progress UPDATE + 一次 insert，百题 200+ 次 RTT；进度又不是强实时需求 | progress 节流（每 N 题或每 2 秒一次），终态 UPDATE 兜底 | test |
| kb_eval_api.rs:702-712 | `llm_text` 无超时：kb_mindmap_api.rs:251 对同类 fast 调用有 20s `LLM_LABEL_TIMEOUT`，这里 LLM 挂起会把 run 卡到进程重启 | 包 `tokio::time::timeout`（出题/评审各一个常量） | test |
| kb_eval_api.rs:709 | `llm.chat(req).await.ok()?`：传输/限流/5xx 全静默成 None，gen_failed/judge_failed 涨了却查不到原因 | `ok()?` 前对 Err 分支 `tracing::warn!(error=%e)` | safe |
| kb_eval_api.rs:564,576 | `item.error` 未过 `sanitize`：检索/答案错误串可能含 `\0` → insert_item 落库被 PG 拒绝 → 整跑假 failed；也无长度上限，且原文透出给空间任何读者 | 落库前 `item.error = sanitize(&item.error, 500)` | test |
| kb_eval_api.rs:761-765 | unfence 只认小写 ```` ```json ````，大写 ```` ```JSON ```` 围栏落到暴力区间抽取（多数也能成，但口径不齐） | `trim_start_matches` 前对前缀做大小写不敏感处理 | test |
| kb_eval_api.rs:782 | `parse_verdict` 剥句读不含 `！`/`!`，"correct!" → None → 误计 judge_failed | 句读表补 `'！','!'` | test |
| kb_eval_api.rs:821 | `clean_question` trim→trim_matches→replace→再 trim 三次全串扫描；且 `trim_matches(['"','\''])` 会把合法以引号开头的问题误剥 | 顺序合并为一次扫描；引号只剥成对包裹的首尾各一个 | test |
| kb_eval_api.rs:832-834 | `sanitize` 的 `replace('\0',"")` + `chars().take().collect()` 两次分配 | 单遍 `chars().filter( | c |
| kb_eval_api.rs:311-312 | INSERT...RETURNING 的 None 分支实际不可达（RETURNING 恒有一行），「评估任务创建失败」是死文案 | 改 `fetch_one`，或注释标注 None 不可达仅兜底 | safe |
| kb_eval_api.rs:368,384,396 | `run.1` 元组下标取 space_id 可读性差；:385 `as_object_mut()` 恒 Some（json! 宏产物必为对象） | run_json 前先 destructure；:385-390 直接构造或 expect | safe |

## store.rs（17 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| store.rs:95-119 | `statements` 只识别裸 `$$`，不认识 `$tag$` dollar-quote；迁移若引入即被切坏（现有测试只钉当前形状） | 支持 tag 形式，或在函数注释显式声明限制并加防御断言 | safe |
| store.rs:84-91 | `migrate` 无并发互斥，多实例同时启动时并发 DDL 可能锁等/失败 | 事务内先 `pg_advisory_xact_lock` 固定键 | test |
| store.rs:762-764 | `grant_space_roles` 的 roles CTE 不滤 btrim 后的空串：传入 `" "` 会插入 `grantee=''` 的 ACL 行（对照 1040-1041 有空串过滤，口径不一） | `WHERE btrim(code)<>''` | test |
| store.rs:785-809 | `grant_space_acl` 的 grantee 未 trim/非空校验，空 grantee 可落库 | 入口校验 `!grantee.trim().is_empty()` | test |
| store.rs:850-986 | `set_status/set_notice/set_enabled/set_doc_source_uri/set_doc_description/set_counts` 六份 SQL 仅字段名不同，整块复制 | 宏/concat! 生成公共 ACL 片段（`&'static str` 约束仍可满足） | safe |
| store.rs:872-874 等 | `n==0 → Forbidden("写权限已失效")` 同时覆盖「文档不存在」，文案对运维有误导（10+ 处同类） | 文案区分「不存在或无权限」 | test |
| store.rs:1018 | 关联上限 50 是裸魔法数，注释/错误文案与判据分散 | 提 `MAX_RELATED_DOCS` 常量，文案复用 | safe |
| store.rs:1072-1074 | `(0*applied.link_changes)` 强制 CTE 求值的技巧无任何注释 | 补一行注释说明为何不能去掉 applied | safe |
| store.rs:1091-1097 | `match state { 1=>Ok(1), -2=>.., _=>Forbidden }`：`_` 把任何意外值都当权限失败，且返回值恒 1 无信息量 | 显式列 -1 分支，其余 unreachable/内部错误 | safe |
| store.rs:1131-1158 | `append_notice` 无长度上限，反复重建失败 notice 无限增长 | 超上限截断（保留最新尾部） | test |
| store.rs:1175-1177 | `rsplit(" > ").next()` 恒为 Some，`unwrap_or("")` 是不可达死分支 | 直接 `next()` 后 trim，删掉 unwrap_or | safe |
| store.rs:1301-1316 | `remap_shadow_embeddings` 单源块 clone 整串 embedding_text/向量字面量 | 返回索引或 Cow，避免大串复制 | safe |
| store.rs:1463 vs 1522 | 注释自承「重跑时 ON CONFLICT 会让 written < chunks.len()」，但 `written==0 → Forbidden`：全量冲突的合法重跑会被误报为权限错误 | 0 行时先 `get_doc`+权限复核再定错误类型 | test |
| store.rs:1550-1581 | `set_embeddings` 丢弃执行行数，调用方无法区分「CAS 全失配」与正常写入 | 返回 `u64` 并让调用方在 0 行时 warn | test |
| store.rs:1694 | `list_docs` 排序只有 `created_at DESC` 无决胜键，与 `list_docs_page`（1742 有 doc_id 决胜）口径不一，同秒插入顺序不稳定 | 补 `, doc_id` 决胜 | test |
| store.rs:1211-1214 | `all(is_some)` 后紧接 `spans[i].unwrap()` 两段式 | 改 `filter_map`+`collect::<Option<Vec<_>>>` 一次完成 | safe |
| store.rs:1256/1275 | 合并预算里 `chunks[p].text.trim().chars().count()` 对每个 pend 块可能重复计算 | 预算环外缓存一次长度 | safe |

## crates/kernel/src/nl/text.rs（17 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/kernel/src/nl/text.rs:12 | `best.as_ref().map(...).unwrap_or(true)` 啰嗦，且每次比较 `chars().count()` 对 best 与 w 各重算一遍 O(len) | `is_none_or`；顺手缓存 best 长度 | safe |
| crates/kernel/src/nl/text.rs:8-23 | `match_word` 返回 `Option<String>` 每次命中至少一次堆分配；返回 `Option<&str>` 即可（name/aliases 生命周期都够），约 10 个调用点机械改 | 改签名 + 调用点 `as_deref()` 清理 | test |
| crates/kernel/src/nl/text.rs:45 vs 52 | R1 用**字符**数（`chars().count() < 2`），R3 用**字节**长（`w.len() > word.len()`）——单位不一；且 contains 已隐含更长，len 检查冗余 | 统一字符口径或删冗余 len 检查 | test |
| crates/kernel/src/nl/text.rs:52 | R3 对全部 words O(n²) 扫描，命中数膨胀时是热点；当前规模无害但无注释 | 注释钉复杂度前提（hits 上限约几十） | safe |
| crates/kernel/src/nl/text.rs:70 | 分隔符表缺全角分号 `；`：`"状态；0=开"` 这类注释会因非 CJK 字整段落入 None，白丢一个本可救回的维度名 | 分隔符串补 `；`（顺带 `。`） | test |
| crates/kernel/src/nl/text.rs:73 | CJK 范围 `\u{4E00}..=\u{9FFF}` 不含扩展 A（U+3400..），生僻字维度名被拒；属保守但无注释 | 注释说明取舍，或扩范围 | safe |
| crates/kernel/src/nl/text.rs:88-105 | `strip_annotations` 遇**未闭合** `（`（注册表文本笔误）时 depth 永不为 0、j 走到尾，其后所有内容被静默吞掉——维护文本一个笔误就丢口径 | depth>0 收尾时改为原样保留该组 | test |
| crates/kernel/src/nl/text.rs:100 | `chars[i..j].iter().collect::<String>()` 每组一次分配只为判 `has_cjk` | 直接 `chars[i..j].iter().any(...)`，确定保留时再 push | safe |
| crates/kernel/src/nl/text.rs:111 | `out.trim().to_string()` 第二次分配 | trim 改切片索引或直接返回（调用方多不敏感首尾空白需核对） | safe |
| crates/kernel/src/nl/text.rs:124 | `sort_by_key(Reverse(w.chars().count()))` 每次比较重算字符数，sort_by_key 每比较一次算一次 key | 先算 `(len, idx)` 再排，或 `sort_by` 缓存 | safe |
| crates/kernel/src/nl/text.rs:126,130 | 每个词一次 `s.replace` 全串重分配（~90 词 × 问句长），可接受但无注释说明为何不上 AC 自动机 | 注释钉「词表规模下 replace 足够」 | safe |
| crates/kernel/src/nl/text.rs:136 | `c.is_alphanumeric() \ | \ | (c as u32) > 0x2E7F`：Rust `is_alphanumeric` 是 Unicode 感知，CJK 表意文字本就返回 true，后一条件冗余；若是双保险则该写注释 |
| crates/kernel/src/nl/text.rs:132-136 | 「纯数字不算残留」依赖**先剥数字再判 alphanumeric** 的顺序，顺序注释只在测试 L244 体现 | 函数注释补一句判定管线顺序承重 | safe |
| crates/kernel/src/nl/text.rs:145-158 | `candidate_windows` 每窗口一个 String（20 字问句 ≈120 次分配）；三个调用方（gather.rs:51、graph.rs:726、warehouse_catalog.rs:833）都只读切片 | 返回 `(usize, &str)` | test |
| crates/kernel/src/nl/text.rs:152-155 | 重复子串产出重复窗口：graph 侧靠 `taken`、gather 侧靠 `seen` 去重，warehouse_catalog.rs:833 侧**没去重**（重复片段重复计分） | warehouse_catalog 侧去重，或函数文档钉「可能重复，调用方自理」 | test |
| crates/kernel/src/nl/text.rs:4 | 搬运源行号引用（`meta.rs:864-918` 等）属易腐注释，源文件已不存在于当前布局 | 统一改成函数名/模块锚点 | safe |
| crates/kernel/src/nl/text.rs:217 | 测试 `expect("缺候选 {w}")` 不插值，失败时打印字面 `{w}` | 改 `unwrap_or_else(\ | _\ |

## scope.rs（17 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| scope.rs:38 | 注释「`ponytail:` 天花板」语义费解，疑似笔误（应为「透明」？），整句警告价值被削弱 | 改写为「谁都能硬写 `new(default(), true)` 撒谎，但那是一行显式、可 grep 的谎」 | safe |
| scope.rs:51 | 「谁都能硬写撒谎」的警告只挂在 struct 注释上，`pub fn new` 本身无 `///` 文档——IDE hover 到 new 看不到警告 | 把警告复制到 `new` 的 doc | safe |
| scope.rs:104-109 | 缓存 miss 时同一用户并发请求各自重跑 ~10s 计算（stampede），`put` 前也不复查 | single-flight（`DashMap` entry 或 `tokio::Mutex` per key），或 put 前再 `get` 一次 | test |
| scope.rs:131-138 | `special_role` 不 trim 输入：`Principal` 字段全 pub，crate 外可构造带空白 `role_code`，特殊角色会静默落到普通路径 | `match role_code.trim()` | test |
| scope.rs:207-209 | `customers_by_area_manager(mysql, &employee_ids)` 在 `employee_ids == [SENTINEL]` 时仍发一次必然为空的 `IN(-1)` 查询（L321 同） | 前置 `if ids == [SENTINEL] { return Ok(vec![]) }` | test |
| scope.rs:321 | `manager_customer_codes` 未经 `clean_strings`（特殊角色路径 L207/L238 都 clean），空串 customer_code 会进 IN 列表 | 包一层 `clean_strings(...)` | test |
| scope.rs:327 | `login_names: vec![p.login_name.clone()]` 未像 L186/215/241 那样 `deny_empty_strings`，五处构造口径不一 | 统一包装或加注释说明「login_name 来自 DB 非空」 | test |
| scope.rs:302-303 | `rows` 两次遍历分别 filter `t==1`/`t==2` | 一次 `rows.iter().partition(...)` | safe |
| scope.rs:270 | `has_real_codes` 硬编码 `"-1"`；全文件 L183/189/192/205/233/262/285/392 共 8 处 `"-1"` 字面量无字符串哨兵常量（kernel 只导 i64 版 `SENTINEL`） | 抽 `const SENTINEL_STR: &str = "-1"` 单点替换 | safe |
| scope.rs:415 | 条件链 `!x.sub.contains(&SENTINEL) && !x.sub.is_empty()`：`is_empty` O(1) 应排在 `contains` O(n) 前 | 调换顺序 | safe |
| scope.rs:399-421 | `customer_codes` 五段查询全部串行 await，段 1/2 与 102/103 及段 5 内部三个查询相互独立，是 ~10s 计算的可压缩时延 | 独立段用 `tokio::try_join!` 并发 | test |
| scope.rs:340 | `decide_base` 的 `PolicyError` 被 `anyhow!("{e}")` 字符串化丢源链（与 lib.rs:45 同一模式） | `.map_err(anyhow::Error::from)` | test |
| scope.rs:376-395 | `employee_codes` 段：`login_names_by_ids` 走 `fetch_str_in`（dms_tables.rs:292）不过滤空串，`out` 只含空串时 L391 的 `out.is_empty()` 为假、不补 `"-1"`，脏串直入 IN | 在 `fetch_str_in` 统一过滤空白（见 dms_tables 条目） | test |
| scope.rs:292 | 注释「与 server/src/scope.rs:85-183 逐条等同」——该文件已删除，引用悬空 | 改为「与 Java DefaultEmployee 逐条等同（原 server/src/scope.rs 已迁本文件）」 | safe |
| scope.rs:66-71 | `manager_names()`、`device_unrestricted_by_role()` 无 `///` 文档，同 impl 其它 getter 都有 | 补 doc | safe |
| scope.rs:123-128 | `SpecialRole` 枚举无 `///`（语义在 L97 函数注释里，hover 枚举看不到） | 补 doc | safe |
| scope.rs:412 | `&[x.p.actual_name.clone()]` 每请求 clone 一次姓名仅为构造临时切片 | `std::slice::from_ref(&x.p.actual_name)` | safe |

## DeepTaskPanel.vue（16 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| DeepTaskPanel.vue:24 | 注释称 STAGE_ORDER「用于进度条比例与当前阶段定位」，但当前阶段高亮（82 行）用的是 `stages.length - 1`，与 STAGE_ORDER 无关，注释与代码不符 | 注释改为「仅用于进度条比例」 | safe |
| DeepTaskPanel.vue:27 | STATE_LABEL 的 queued 译作「入列」，与 KbEval.vue:134 同款状态译作「排队中」不一致，且「入列」生僻 | 统一为「排队」/「排队中」 | safe |
| DeepTaskPanel.vue:36,82 | 魔法字符串「处理失败」两处硬编码，后端措辞一变两处需同步 | 抽 `FAILED_STAGE` 常量 | safe |
| DeepTaskPanel.vue:44 | 失败中断时 `!running → 100%`，进度条跳满再变红，视觉上像「跑完了」 | failed 时冻结在最后一次百分比 | test |
| DeepTaskPanel.vue:45-46 | 魔法数 6 / 96 无命名无注释 | 抽 `MIN_PERCENT/CAP_PERCENT` 常量 | safe |
| DeepTaskPanel.vue:49 | `elapsed` 秒数直出「12s」，与 fmtMs 的「12.3s」格式不一致；且 elapsed 单位（秒）在 props 类型上无注释 | props 注释标明单位；显示统一一位小数或整数 | safe |
| DeepTaskPanel.vue:50 vs 108 | 失败态头部叫「已中断」，折叠摘要叫「分析中断」，两处措辞不一 | 统一措辞 | safe |
| DeepTaskPanel.vue:53-61 | 「完成后自动折叠」只靠 loading 由 true→false 的 watch 跳变触发；切视图再切回（`:key` 重挂载且 loading 已为 false）时折叠状态丢失，面板重新展开，与注释承诺不符 | `collapsed` 初始值取 `!props.turn.loading` | test |
| DeepTaskPanel.vue:63-65 | `fmtMs` 未取整，`ms=123.456` 显示「123.456ms」 | `Math.round(ms)` 后拼接 | safe |
| DeepTaskPanel.vue:70 | 折叠头是 `<div @click>`，无 `role="button"`/`tabindex`/`aria-expanded`/键盘事件，键盘与读屏不可用 | 改 `<button type="button">` 或补全 ARIA 与 keydown | safe |
| DeepTaskPanel.vue:86 | 占位阶段「理解问题与业务口径」无条件带 `current` 高亮：已结束但 progress 为空（仅 tasks 有数据）的回合也会显示一个蓝色的「进行中」假阶段 | 占位行 `current` 加 `running &&` 条件 | test |
| DeepTaskPanel.vue:96 | `:key="task.title"`，两个板块同名（如重复主题）时 key 冲突、状态点串行 | key 改用 `` `${i}:${task.title}` `` | safe |
| DeepTaskPanel.vue:101 | `title` 属性与可见文本完全重复（无截断时），鼠标悬停出冗余 tooltip | 仅在截断时给 title 或移除 | safe |
| DeepTaskPanel.vue:103 | `STATE_LABEL[task.state] ?? task.state`：STATE_LABEL 已是全联合 Record，`??` 分支类型上不可达；若真防御未知 state，不如把未知值也渲染成徽章样式而非裸文本 | 保留防御但补注释「防御老服务端新状态」 | safe |
| DeepTaskPanel.vue:108 | 折叠摘要在「既非 failed 也无 tasks」时显示「分析完成」，对从未跑起来的回合（progress 有但 tasks 空且失败前期）可能误报完成 | 无 tasks 且 stages 不含「完成」时显示「已结束」 | test |
| DeepTaskPanel.vue:37-38 | `tasksDone`/`tasksFailed` 两次 filter 遍历同一数组 | 合并为一次 reduce（量小，仅为可读性） | safe |

## docker/server/Dockerfile（16 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| docker/server/Dockerfile:9 | `FROM rust:1-bookworm` 浮动 tag，Rust 小版本每月漂，同一锁树 rebuild 出不同产物，出问题查不出原因 | 钉 `rust:1.xx-bookworm` 或 digest（与 L13 `--locked` 的可复现口径对齐） | test |
| docker/server/Dockerfile:9 | 容器工具链与 `rust-toolchain.toml`（`stable-x86_64-pc-windows-gnu`）是两套，全文无交代；后来人若把 rust-toolchain.toml COPY 进来，rustup shim 会在容器里尝试装 windows-gnu 通道直接炸 | L9 旁加一行注释：「容器走镜像自带 Linux stable，rust-toolchain.toml 只管宿主机，别 COPY」 | safe |
| docker/server/Dockerfile:11-13 | 无依赖预构建层、无 BuildKit cache mount：改任何一行 crates 源码 → `COPY crates` 层失效 → 7 crate 全锁树从头重编 | `RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/ws/target cargo build --locked`（需 BuildKit，serve.ps1 构建命令不受影响） | safe |
| docker/server/Dockerfile:13,18 | `cargo build` 无 `--release`，L18 从 `target/debug` 拷——容器跑的是 debug 构建（慢、体积大、无优化），且全文件无一句取舍注释（与本仓库注释密度反差明显） | 二选一：加注释写明「debug 是有意取舍（构建快）」，或切 `--release` 并同步改 L18 路径 | test |
| docker/server/Dockerfile:13,18 | profile 字样硬编码在两处（build 命令与 debug 路径），改 profile 必须同步改两处且无任何提示 | 抽 `ARG PROFILE=debug` 或至少在两行互加「改我必改另一处」注释 | safe |
| docker/server/Dockerfile:16 | `ca-certificates` 很可能多余：reqwest 用的是 `rustls-tls`（根 Cargo.toml:28，webpki-roots 内置根证书，不读系统 CA store） | 实测一次 HTTPS 出站（LLM 端点若为 https）后删除，省一次 apt 事务 | test |
| docker/server/Dockerfile:17 | `WORKDIR /app` 是 serve.ps1:54-57 的隐性契约（`why-not-compose` 读相对路径 `tools/eval_cases.json` → 解析成 `/app/tools/...`），Dockerfile 侧无注释，改 WORKDIR 会静默断诊断 | L17 加注释：「/app 是 serve.ps1 挂载契约的一部分，勿改」 | safe |
| docker/server/Dockerfile:19 | 注释「知识库落盘目录：挂 volume」措辞过时：实际方案是 bind mount 宿主目录 `D:\kbdata→/kbdata`（serve.ps1:53），不是 docker volume | 注释改为指向 serve.ps1 的现行方案 | safe |
| docker/server/Dockerfile:20 | `mkdir -p /app/kb_data` 是死代码：全仓 grep 无任何代码引用 `kb_data`（默认 kb_root 是 `data/kb`，容器部署是 `/kbdata`）；serve.ps1:41 只把它当「坏方案」举例 | 删 mkdir，或改成 `mkdir -p /app/data/kb`（对齐 settings.example.json:16 的默认相对路径落点） | safe |
| docker/server/Dockerfile:5,8 | 注释说「settings.json 运行时挂载」「密钥只在 settings.json」，实际挂的是 `settings.docker.json` 改名进容器（serve.ps1:52）——同段两个文件名混称 | 注释点明「宿主侧文件名 settings.docker.json，容器内叫 settings.json」 | safe |
| docker/server/Dockerfile:15-22 | 无 `USER`，服务以 root 跑且挂的 D:\kbdata 可写 | 加非 root 用户（bind mount 的宿主目录写权限要实测，故带测试） | test |
| docker/server/Dockerfile:22 前 | 无 HEALTHCHECK：serve.ps1:95-102 只能靠外部轮询 `/api/health` 90 次判活 | 内建 HEALTHCHECK（slim 无 curl，可用 `bash -c '</dev/tcp/127.0.0.1/8100'` 式一行） | safe |
| docker/server/Dockerfile:21 | `EXPOSE 8100` 硬编码，与 settings.docker.json:6 `listen` 解耦——listen 改端口后 EXPOSE 成误导 | 注释一句「EXPOSE 仅文档，实际以 settings 的 listen 为准」 | safe |
| docker/server/Dockerfile:15-22 | 可分发镜像无 OCI LABEL（source/revision），`docker inspect` 查不到出处 | 加 `LABEL org.opencontainers.image.source="https://github.com/caowenkai1121-dotcom/dms-ai"` | safe |
| docker/server/Dockerfile:22 | 无停机语义说明：docker stop 默认 SIGTERM + 10s 强杀，in-flight 写库行为无任何注释 | 显式 `STOPSIGNAL SIGTERM`（本就是默认，纯文档化）+ 一行注释 | safe |
| docker/server/Dockerfile:1-8 | 顶部 SAC 叙事与 docker/parser/Dockerfile:1-6 同一件事两处写法不一（一边一句带过、一边六行带实测数据），长期易漂 | 一边留全文，另一边改为「理由全文见 docker/parser/Dockerfile 头注」 | safe |

## quality_api.rs（16 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| quality_api.rs:92 | 500 把 sqlx 原始错误 `e` 透给客户端（PG 错误文案含约束名/表名等内部信息）；kb_api、kb_eval_api 已统一"…暂时不可用"通用文案先例 | `tracing::warn!(err=%e, ...)` 落日志，返回通用文案 | safe |
| quality_api.rs:130,139,150 | `quality` 三条查询同样 `err(500, e)` 原生透库错误 | 同上：warn + 通用文案 | safe |
| quality_api.rs:189 | `set_feedback_status` 同样 `err(500, e)` 透原生错误 | 同上 | safe |
| quality_api.rs:109,140-149 | `FeedbackRow` 后三列（login_name/question/route）来自 `LEFT JOIN LATERAL`，feedback 对应的 query_log 行被保留期清理后为 NULL，非 `Option<String>` 元组解码直接报错 → 整个 quality 端点 500 | 三列改 `Option<String>`，或 SQL 侧 `COALESCE(q.login_name,'')` 等 | test |
| quality_api.rs:83 | `ON CONFLICT DO UPDATE SET created_at=now()` 覆盖首次反馈时间：列名 `created_at` 与"最后修改时间"语义不符，且重提交会把旧反馈顶到 :148 `ORDER BY created_at DESC` 列表最前 | 不更新 `created_at`（或至少注释写明"重提交顶贴"是有意设计） | test |
| quality_api.rs:65,86 | UUID 只校验不归一化：客户端发大写/无连字符变体 `parse_str` 能过，但绑定原文与库中小写标准形不等 → 白白重试 400ms 后 404 | 用解析结果 `Uuid::to_string()` 归一化后绑定 | test |
| quality_api.rs:118-150 | 三条互不依赖的只读查询串行 await，白付两个 RTT | `tokio::try_join!` 并行 | test |
| quality_api.rs:118,129 | 聚合查询恒返一行，`fetch_optional + unwrap_or` 的 None 分支是死代码 | 改 `fetch_one`，删掉 9 元组默认值 | safe |
| quality_api.rs:97 | 404 文案"请稍后重试"：query_log 已被清理/永不存在时重试无意义，误导 | 文案拆两种情形或改为"记录不存在或已过期" | test |
| quality_api.rs:20,68 | `kind` 闭集在 DDL CHECK 与 handler `matches!` 两处硬编码；测试 :200-206 只钉了 DDL 一侧，handler 侧漂移无人守 | 测试加 handler 源码锚点（含五个 kind 字面量） | safe |
| quality_api.rs:22,184 | `status` 闭集同样在 DDL CHECK 与 :184 `matches!` 两处硬编码，无锚点测试 | 同上补锚点断言 | safe |
| quality_api.rs:72 | 重试预算 `[0,40,120,240]` 裸魔法数，总预算 400ms 无注释；为何这四个值无出处 | 提常量数组 + 一行总预算注释 | safe |
| quality_api.rs:12,37-39 | `ApiErr` 别名 + `err()` 与 usage_api.rs:55-59、trace_api.rs:76-80 三处逐字重复 | 抽 `crate::api_err`（或放进现有公共模块）共享 | safe |
| quality_api.rs:107,151-158 | `SummaryRow` 9 元组全位置访问（`.0`–`.8`），列序错位无编译保护，读性差 | 改 `sqlx::Row` 按名取或具名结构体 | safe |
| quality_api.rs:30-35 | `migrate` 三句 DDL 无事务，中途失败留半迁移；幂等性依赖"重启重跑"这一隐式事实，无注释 | 包一个事务，或注释写明幂等靠重跑 | safe |
| quality_api.rs:63 | 401 文案"未认证"与 usage_api.rs:154、trace_api.rs:206 的"未认证：缺会话 token 或 login_name"不一致 | 统一文案 | safe |

## trace_api.rs（16 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| trace_api.rs:5,8-11 | 头注仍写"路由注册由集成方在 main.rs 补 / 接线后删 #[allow(dead_code)]"，但 main.rs:1340 已注册——注释与代码不符（main.rs:30-31 usage_api 上方注释同样过期） | 更新头注为"已接线"，删接线指引 | safe |
| trace_api.rs:51-52,757-759 | `msg_payload` 头注与测试注释仍写"本次不注册 main.rs"，但 main.rs:1341 已注册 | 同上更新 | safe |
| trace_api.rs:209-216 | conv 不存在走 `Ok(None)` → 403"无权访问该会话"；而 msg_payload 对不存在的 msg 给 404（:239）。同类"资源不存在"两处口径不一，若 403 是防枚举的有意设计则未写注释 | 注释写明取舍，或统一口径 | safe |
| trace_api.rs:90-109 | `MSGS_SQL` 对 user 行也跑 `jsonb_build_object`：payload 为 NULL 时产出 `{"route":null,...}` 对象而非 SQL NULL → `MsgRow.payload` 是 `Some(全null对象)`，与 :137、:54"user 行 payload 为 null"语义不符（assemble 不读 user payload 故无行为差异，属语义漂） | 外层 `CASE WHEN payload IS NULL THEN NULL ELSE jsonb_build_object(...) END` | test |
| trace_api.rs:217-218 | `fetch_msgs` 与 `fetch_failed` 互不依赖却串行 await | `tokio::try_join!` | test |
| trace_api.rs:90,121 | 两条查询都无 LIMIT：超长会话的消息与失败行一次全拉回，响应体无界（payload 已投影，行数本身仍无界） | 各加 LIMIT（如 500）+ 超量截断标记 | test |
| trace_api.rs:209-216,243-250 | 属主闸门 match 块（三臂 + 两条文案）在两 handler 逐字重复，漂移风险（现靠锚点测试守） | 抽 `ensure_owner(pool, conv_id, &login)` helper | safe |
| trace_api.rs:341,346,420 | `question.clone()` 与 `asked_at.to_rfc3339()` 各算两次（Question 事件一次、Round 字段一次） | 先各算一次再复用 | safe |
| trace_api.rs:430-444 | `interrupted_round`：`u.question.clone()`、`u.at.to_rfc3339()` 各两次 | 同上 | safe |
| trace_api.rs:448-468 | `failed_round`：`f.question.clone()`、`f.status.clone()`、`f.at.to_rfc3339()` 各两次 | 同上 | safe |
| trace_api.rs:358,372 | `as_u64 → min(i64::MAX as u64) as i64` 同一夹取写两遍 | 抽 `fn clamp_ms(v:&Value)->Option<i64>` 小助手 | safe |
| trace_api.rs:307 | `timed` 未预分配，容量上界 `msgs.len()+failed.len()` 已知 | `Vec::with_capacity` | safe |
| trace_api.rs:362-367 | 缺 stage/kind 的畸形 steps 条目静默 `continue`：数据是自己落库的，出现畸形=上游形态已变，无任何留痕 | 加 `tracing::debug!` 记一条 | safe |
| trace_api.rs:371 | steps 缺 `ms` 折成 0，但头注 :36 声明"`null` = 没有可归属耗时"——`Route.ms` 是 i64 无法为 null，"0ms"与"无计时"对前端不可区分，注释没补这一例外 | 头注补一句"steps 缺 ms 记 0" | safe |
| trace_api.rs:381 | `route.contains("+repair")` 子串匹配：未来出现如 `xx+repairy` 类路由值会误命中 | `route.split('+').any( | s |
| trace_api.rs:234-240,268-276 | `Row::get` 在列缺失/解码失败时直接 panic（如类型漂移），worker panic 而非干净 500 | 改 `try_get(...).map_err( | e |

## triage.rs（16 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| triage.rs:51-58 | `normalize_typos` 先 `any` 扫一遍、命中后 6 对全量 `replace` 各扫一遍（共最多 7 次全串扫描）；干净问句零分配已守住，但命中路径可一趟完成 | 命中路径只对 `any` 命中的那对做 replace（或保持现状并在注释说明 6 对量级无所谓）；行为等价 | safe |
| triage.rs:69-74 vs 219 | `time_hit` 调 `time_tokens(q).is_empty()`——为判空构了一整棵 `BTreeSet`（堆分配），每次分诊/规则判定都付一次 | 把词表提为 `const TIME_WORDS`，`time_tokens` 与 `time_hit` 共用，`time_hit` 用 `iter().any`（单一事实源不动） | safe |
| triage.rs:120 vs 130 | `rule_intent(question, false, false)` 返回 None 后，130 行 `rule_intent(question, data, kb)` 把 time_hit/analytical/table/doc 四组扫描原样重算一遍（纯函数同输入同输出） | 四组内部信号在 `triage()` 里算一次，或给 `rule_intent` 加个内部预计算入口；判据不变得行为等价 | safe |
| triage.rs:126 | `tracing::warn!("triage: 注册表召回失败（{e}）→ data")` 用内联格式化；ask.rs:271/305 同类日志都是结构化 `err = %e` 字段 | 改 `tracing::warn!(err = %e, "...")`，与全仓日志风格统一 | safe |
| triage.rs:149 | both-hit 的 info 日志不带问句——分诊排障最需要的就是「哪句话两侧都命中」，现日志无此信息 | 加 `question` 字段（与 ask.rs:263 同风格） | safe |
| triage.rs:162-170 | OBJECTS 收 `"SKU"`/`"sku"` 不收 "Sku"；TARGETS 收 `"top"`/`"TOP"` 不收 "Top"——而 `kb_hit`（186）是先 lowercase 再匹配的，同文件大小写策略两套 | 判定前统一 `to_ascii_lowercase` 一次，词表只留小写；命中面变化需测试 | test |
| triage.rs:171 | `const RELATIONS: &[&str] = RELATION_WORDS;` 纯转发别名，多读一次定义 | 180 行直接用 `RELATION_WORDS`，删别名 | safe |
| triage.rs:186 | `kb_hit` 对纯中文问句也整串 `to_lowercase()` 堆分配；KB_WORDS 里需要小写化的只有三个 ASCII 扩展名 | 非 ASCII 词直接 `question.contains(w)`，ASCII 词惰性 lowercase（或用 `to_ascii_lowercase`，对现词表等价） | safe |
| triage.rs:236 | `table_hit` 用 Unicode `to_lowercase()`，但判据只看 ASCII（`t_` + 小写字母）——`to_ascii_lowercase` 更便宜且语义等价 | 换 `to_ascii_lowercase()` | safe |
| triage.rs:247-254 | `doc_code_hit` 把「数字+连字符」当单号：`"2025-01-01"`（带横杠日期）token 含数字且含 `-` → 命中；注释只排除了「纯数字串」，带杠日期这一族漏了——含日期的制度类问句会被抢成 Data | 要求 token 含 ASCII 字母（而非仅 `-`），或排除 `\d{4}-\d{2}-\d{2}` 形；判据变化需测试 | test |
| triage.rs:262-266 | fast LLM 的**传输/调用错误**被 `.ok()?` 静默吞掉，只有超时有 warn（264）——「模型挂了」与「超时」在日志里不可区分 | 内层 Err 也补一行 warn（或 `inspect_err`） | safe |
| triage.rs:261 vs ask.rs:740 | 同为 fast 分类任务，triage 二分类用温度 0.1、ask 三词门用 0.0，两处无说明 | 统一或在注释说明各自取值理由；温度变化影响判定需测试 | test |
| triage.rs:277-281 | `parse_intent` 用 `contains("data")`：LLM 回 "metadata"/"database" 会误判 Data；tolerant 解析对 LLM 路是刻意的，但同一函数也服务 forced chip（108）——chip 值本该精确匹配 | forced 路径改精确等值匹配，LLM 路径保持 tolerant；需测试 | test |
| triage.rs:95-135 | `triage()` 里 `kb_hit`/`entity_form_hit`/`rule_intent` 吃的是归一后的问句，但 149 行 both-hit 日志（若补上问句）与 134 行 LLM 兜底拿到的也都是归一版——文档注释（48-49）说「路由出去的原问句不动」，建议在该行附近再钉一句「LLM 兜底见的也是归一版」，防后人误以为 llm_intent 收原句 | 注释补一句即可 | safe |
| triage.rs:302-309 / 462-469 | 两个源码扫描测试各自手撕 `"pub async fn triage"` 取函数体，切片逻辑重复两遍 | 抽 `fn triage_body(src: &str) -> &str` 测试辅助 | safe |
| triage.rs:218-230 | `time_hit` 的「数字+年/月/日/号/季」要求数字紧邻：「2024 年」（带空格）不命中——与注释示例 `"2024年1月"` 一致，属已知边界，建议注释里明说「空格分隔不算」免得后人当 bug 修 | 注释补边界说明 | safe |

## seed.rs（16 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| seed.rs:86-123 | `seed_warns` 的 UPDATE 不查 `rows_affected`，表名打错时 0 行静默（同文件 `seed_table_comments` 181-192 已有 missed+warn 模式，这里不一致） | 仿 181-192 收集 0 行表名并 `tracing::warn!` | safe |
| seed.rs:102 | `t_winc_stock_transfer` 的 warn 文案写「才考虑 t_winc_sale_report（有 deleted_flag)」——库存流水表指向销售报表，疑似从 101 行复制未改 | 与业主核对后改为指向库存侧表 | test |
| seed.rs:188-191 | warn 文案「没落到任何行」在**部分**表未命中时也触发，措辞误导排查方向 | 改为「以下表注释修正未命中任何行： {missed}」 | safe |
| seed.rs:226-238 | 文档注释说判「多张不同族表共用」，SQL 却 `HAVING count(*) >= 3`——恰好 2 张异族表共用注释时守卫不开火，注释与行为不符 | 阈值改 2 或把注释写清「≥3 张才报」 | test |
| seed.rs:258-260 | `family_of` 每次调用都 `collect::<Vec<_>>().join()` 分配 String；244 行循环里对每张表调一次 | 改返回 `&str`（`split('_').take(2)` 用 `next` 两次拼切片），`HashSet<&str>` 接收 | safe |
| seed.rs:296-299 | 软删登记 SQL 里 dimension 侧有 `status='active'` 过滤，metric 侧没有——已禁用指标的来源表仍会被登记 `deleted_flag = 0` 口径 | metric 子查询补 `AND status = 'active'`（或注释说明有意为之） | test |
| seed.rs:295 | `split_part(trim(source_table),' ',1)` 只取第一个表：`"t_invoice_apply_header UNION ALL t_invoice_new_apply_header"`（seed_defs.rs:96）的新表永远拿不到软删登记 | 对 `UNION ALL` 形态也剥出第二表（regexp_split_to_table） | test |
| seed.rs:319-324 | fail-closed DELETE 的关键词清单手抄了 KW_FORCE 里销售/订单系 7 个词（347-349 行），新增核心词时两处会漂 | 从 `KW_FORCE` 中 table 属于 `sales_fact::TABLE_NAME`/`t_sales_order` 的项派生 DELETE 列表 | safe |
| seed.rs:354-364 | KW_FORCE 40+ 行逐行单条 INSERT，每次启动 40+ 次 round trip（`seed_warns`、`seed_pitfalls`、`seed_join_edges` 同构） | 用 `UNNEST($1,$2)` 一次批量 upsert | safe |
| seed.rs:390-391 | 「JOIN 边种子…」的 doc comment 错挂在 `seed_table_scopes` 头上，且与 391 行重复/矛盾；真正的 `seed_join_edges`(453) 反而无注释 | 把 390 行移到 `seed_join_edges`，删重复行 | safe |
| seed.rs:395,418,443,511 | `seed_table_scopes`/`seed_table_snapshots`/`seed_value_domains`/`seed_join_edges` 的 INSERT 不写 `ds_id` 列，靠 DDL DEFAULT 'dms'，而 ON CONFLICT 却点名 ds_id；346 行注释自己刚说「显式写 ds_id」 | 四处统一显式 `.bind(DMS_DS_ID)` | safe |
| seed.rs:70-82 | `invalidate_stale_exemplars`：`metric_versions` 尾逗号或空段产生 `v=''`，`split_part('','@',1)=''` 在 LEFT JOIN 下 `m.metric_code IS NULL` 成立 → 整条样例被误判 stale | `unnest(...)` 后加 `WHERE v <> ''` 或 `nullif` 过滤 | test |
| seed.rs:530-579 | `seed_pitfalls` 用 `WHERE NOT EXISTS(trigger_words,lesson)` 去重：教训文案一改，旧行不更新不删除，新旧两条同时 active 参与召回（其它种子都是 ON CONFLICT DO UPDATE） | 给 pitfall 加唯一键改 upsert，或对种子集做「不在清单即停用」 | test |
| seed.rs:15-38 | `seed()` 二十余步无事务包裹，中途失败留下半灌状态（幂等可自愈，但首次启动半灌+无日志难查）；全程无一条完成/耗时日志 | 包一层事务或至少在末尾 `tracing::info!` 汇总 | test |
| seed.rs:584-598 | `seed_datasources` 用 `DO NOTHING`：DESC 文案（向量选源唯一素材）在源码里改进后，存量库永远拿不到新文案；注释只解释了 name/description 不应被冲回，没说 DESC 怎么演进 | 加「description 等于上一版种子值时才更新」的条件更新 | test |
| seed.rs:205 | 测试里 `src.split(&format!("async fn {f}"))` 循环内重复分配；且 `format!` 锚点可提为循环外 | 小事：锚点数组提到循环外 | safe |

## metric.rs（16 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| metric.rs:74-77 | 12 元组类型写两遍（Vec 标注 + turbofish），加字段要改三处 | type alias 或 `#[derive(FromRow)]` struct | safe |
| metric.rs:100 | `\ | i\ | rows[i].0.clone()`：sort 每对比较两次 String 深克隆 |
| metric.rs:61-65 | `chars().count()` 每对比较重算两遍 | 预计算键 `sort_by_cached_key((Reverse(len), name))`，全序等价 | safe |
| metric.rs:106-107 | `rows[matched[k].0].clone()` 整 12 元组克隆——`aliases: Vec<String>` 深克隆后当 `_a` 丢弃 | 按字段解构引用，只克隆所需字段 | safe |
| metric.rs:68-93 | 全表扫 meta.metric 无缓存：同一问句至少 2 次（gather 波 1 gather.rs:72 + corrector.rs:559），术语递归每命中一个术语再 +1（cards.rs:167） | 行集作参数共享/请求级缓存（新鲜度窗口变化需评审） | test |
| metric.rs:69-73 | `ds_pred(1)+source_asset_live_pred_at("",1)` 每次调用 format!/replace 重拼同一常量串（全部召回 SQL 同族） | OnceLock 或 const 化谓词片段 | safe |
| metric.rs:167 | `agg_expr.to_uppercase()`：Unicode 全串分配；判据目标 SELECT 是纯 ASCII | `to_ascii_uppercase()` | safe |
| metric.rs:167 | 复合判据「含 SELECT 字串」：agg_expr 的字符串字面量里出现 select 字样（如 `CASE WHEN x='select'`）会误判 → 多渲「严格照抄」句 | 注释钉启发式前提，或词边界判据 | test |
| metric.rs:161-165 | `time_cap` 精确匹配 `"yesterday"`：种子写 "Yesterday"/带空格静默不渲那句 ⚠️ | `trim().eq_ignore_ascii_case("yesterday")` 或注释钉契约 | test |
| metric.rs:20 | 模块注释「7 个断言原地留在本文件」——现 map_filter_* 5 测试 6 断言 + match_word 2 断言 = 8，「7」对不上任何口径 | 改计数或不写死数字 | safe |
| metric.rs:35-37 | time_cap 字段注释「指标级时间窗上限…」与「指标级数据新鲜度上限…」语义重复 | 去重 | safe |
| metric.rs:133-135 | `metric_card = metric_card_for("", m)`：依赖「""≠DMS_DS_ID → 原样返回」（registry/mod.rs:209-211）的隐式契约，无注释 | 补一行注释 | safe |
| metric.rs:179 | 单行 12 占位 `format!`，占位与实参对位靠数逗号 | 分段 push 或命名占位 | safe |
| metric.rs:190-193 vs 197-199 | doc 举的例子是「同词」场景，判据实为「等长」：命中词字数相同但词不同的落选者也出 chip，注释未覆盖 | 注释补「判据是等长不是同词（有意近似）」或改同词判据 | safe |
| metric.rs:87-92 | 给 `catalog_allows_metric_record` 连传 10 个同类型参数：顺序错位编译器查不出，且无测试钉传参顺序 | 无库断言造一个字段两两不同的样本过该判据 | test |
| metric.rs:68 | 失败语义不统一：`?` 传播 → corrector.rs:559 硬失败，gather 波 1 则 warn+缺席；fn doc 未写「谁可以 `?` 谁必须降级」 | fn doc 钉失败语义契约 | safe |

## datamap_usage.rs（16 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| datamap_usage.rs:192 | ds_id 未 trim/小写归一，query_log 里 'DMS' 与 'dms' 会裂成两组各自归一化/落库；与 datamap.rs:850、lineage.rs:470 口径不一 | `ds.trim().to_ascii_lowercase()` 后再判空归 'dms' | test |
| datamap_usage.rs:243-249 | `max` 在过滤裸列对之前计算：带裸列的列对（最终 256 行被丢弃）其 freq 仍计入 max，把可落库边的 norm 系统性压小 | 先过滤可归属列对，再在幸存集合上算 max | test |
| datamap_usage.rs:79-85 | co_occurs upsert 的 SET 不含 `updated_at`（datamap.rs:832、lineage.rs:462 都刷），该列对 usage 边永久停在插入时刻 | SET 里补 `updated_at = now()` | test |
| datamap_usage.rs:45 | 注释「单轮回写上限」与代码不符：`truncate(MAX_EDGES_PER_RUN)` 在 262 行 per-ds 循环内，实际单轮上限 = 500×ds 数 | 注释改「每数据源回写上限」，或把截断移到全局 | safe（改注释） |
| datamap_usage.rs:128-142 | `edges_upserted` 计数器与 `edges.len()` 恒等（中途失败直接 Err 返回，永远走不到 143） | 报告直接填 `edges.len()`，删计数器 | safe |
| datamap_usage.rs:129-141 | 逐行 upsert 无事务，与 datamap/lineage 同一形态问题 | 包事务提交 | test |
| datamap_usage.rs:487-534 | `collect_cols` 不覆盖 `Expr::Case`（SELECT 里 CASE WHEN 极常见）、HAVING、GROUP BY、ORDER BY，同现列对漏收 | 补 `Expr::Case`/`HAVING` 遍历（注释 486 已声明只 SELECT/WHERE，改动需同步头注 5 行口径） | test |
| datamap_usage.rs:420-425 | JOIN ON 只认 Inner/Left/Right/Full Outer，`LeftSemi`/`LeftAnti`/`CrossApply` 等直接落 None 走结构兜底，可能误配 | 按需补臂或在注释里写明「其余 JOIN 类型一律结构兜底」 | safe（补注释） |
| datamap_usage.rs:253/256-258 | 每条边克隆 4-6 个 String（排序键一份 + 落库四元组一份） | 排序键改用索引或 `Rc<str>`；微优化，量小可不动 | safe |
| datamap_usage.rs:102-106 | `ParseFailure` 只有 `Clone`，无 `Debug`，日志/断言里打印不便 | 补 `#[derive(Debug, Clone)]` | safe |
| datamap_usage.rs:88-99 | `UsageReport` 无 `Debug`，CLI 侧想 `{:?}` 打印要手写 | 补 `#[derive(Debug)]` | safe |
| datamap_usage.rs:109-157 | 校准完成无 info 日志（仅失败时 125 行 warn），与 lineage.rs:549 不一致 | 末尾加 `tracing::info!(rows, edges, "校准完成")` | safe |
| datamap_usage.rs:124-126 | 解析失败 warn 只有总数，不带 per-ds 分布，多数据源时定位难 | warn 里带 `by_ds` 各源失败数（或 debug 级明细） | safe |
| datamap_usage.rs:659 | 测试注释写「唯一键 (src,dst,kind) 约定钉死」，是旧 schema 残留；实际六元组 `(ds_id,kind,left_table,left_col,right_table,right_col)`（663-671 断言的就是六元组） | 注释改为六元组 | safe |
| datamap_usage.rs:114 | `sql <> ''` 放行了全空白 SQL，这类行进 parse 必然失败，虚增 parse_failure_total | 查询侧加 `trim(sql) <> ''` 或在 aggregate 里 skip 全空白（口径变化需知会） | test |
| datamap_usage.rs:169-173 | `by_name("mysql").expect(..)`/`by_name("pg").expect(..)` 每行日志调用两次注册表查找 + expect | 函数入口各取一次存局部（或 OnceLock），循环外不变 | safe |

## guard.rs（16 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| guard.rs:215 | 注释「非纯聚合且无 LIMIT → 追加」与实现不符：216-229 只判已有 LIMIT，完全没有聚合判定（agent/gate.rs:108 抄了同一句错话） | 注释改成「无 LIMIT/FETCH → 追加」 | safe |
| guard.rs:217,225 | 217 注释宣称「字面量含 limit 不再误判」，但 225 的 fallback 正是 `to_uppercase().contains("LIMIT")`——parse 失败时字面量/注释里的 LIMIT 照样误判为已限流（漏判=无界扫描） | fallback 先过 `strip_literals_and_comments` 再 contains；或注释承认 fallback 不享受该保证 | test |
| guard.rs:60 | `replace("for update"," ")` 只认单个空格：`FOR\nUPDATE`/`FOR  UPDATE`（多空白）不匹配 → 落入 `WriteToken("update")`，拒绝理由与 58 行注释宣称的「调用方按行锁拒」不一致 | 用空白归一化（split_whitespace join）后再 replace | test |
| guard.rs:42 | 空 SQL（0 条语句）也报 `MultiStatement`，文案与「多条语句」语义不符 | 0 条时给独立变体或复用 `Parse`；至少注释说明 | safe |
| guard.rs:49 | `sql.contains("/*!")` 查原文含字面量：`'价格 /*! 说明'` 类字面量被误拒（多拒方向但未在注释声明） | 注释声明该误伤方向；或先判字面量剥离后残余 + 原文双轨 | test |
| guard.rs:35,59 | parse 失败分支与主路径各做一次 `strip_literals_and_comments().to_lowercase()`，逻辑重复两份 | 抽一个小 helper（如 `fn lowered_stripped(sql)`） | safe |
| guard.rs:60 | 两次 `.replace(...)` 产生两个新 String；可一趟完成 | 合并为一次扫描或接受现状并在注释说明量级 | safe |
| guard.rs:87-96 | `forbidden_token` 对每个 token 线性扫 17 词表 | FORBIDDEN 排序后 `binary_search`，或 `matches!`——行为等价 | safe |
| guard.rs:113-117 | `system_schema_ref` 纯 `contains`，无左边界：`"oldmysql.t"`、`"akb.t"` 这类库名含 `mysql.`/`kb.` 子串即被误拒 | 命中位置前一字符须非 `[a-z0-9_]`（与 111 行注释宣称的「`库名.` 限定形态」对齐） | test |
| guard.rs:122-124 | `sensitive_ref` 的 needle 不归一大小写：stripped 已 lowercase，若业务侧词表混入大写条目则静默漏拦 | 入口把 sensitive 逐项 lowercase（或 `const` 断言全小写） | test |
| guard.rs:130-136 | 占位符扫描 `sql.split('\'')` 不理解 `\'` 转义：`'it\'s __X__'` 的碎片不以 `__` 开头 → 漏判 | 用与 lex 一致的引号状态机扫字面量段 | test |
| guard.rs:137 | `lower.contains("_placeholder")` 会误伤合法列名（如 `is_placeholder`、`placeholder_flag`） | 收窄到字面量段内匹配，或注释声明该误伤方向 | test |
| guard.rs:128-141 | 注释段也参与扫描：`'__X__'` 写在一行注释里同样触发 UnfilledPlaceholder（多拒，未声明） | 先剥注释再做占位符扫描 | test |
| guard.rs:187,197-213 | `references_any_table(stmt)` 在 `constant_projection` 里对整条语句做第二次全量 Visit；传入的 `q` 已可用 | 改为 visit `q`（语义相同），少一层 Statement 包装遍历 | safe |
| guard.rs:216-229 | `ensure_limit_with` 对同一 SQL 再 parse 一次；`gate::check` 一条 SQL 共 parse 三次（is_safe / ensure_limit / table_names_of） | 让 is_safe 返回/透传已解析 AST，后续复用 | test |
| guard.rs:227 | 未改动时仍 `sql.to_string()` 复制一份 | 返回 `Cow<'_, str>`（签名内改，行为同） | safe |

## owned.rs（16 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| owned.rs:11 | 头注「唯一的例外是 `pool()`」漏了 `dead_pg_pool_for_tests`（44 行）同样对外交出裸 `PgPool`——注释与代码不符 | 头注补一句测试死池例外 | safe |
| owned.rs:44 | `dead_pg_pool_for_tests` 是 `pub` 且随 release 编译（lib.rs:30 无 cfg 门），仅靠注释约束「只给单测用」 | 加 `#[doc(hidden)]` 标明非生产 API | safe |
| owned.rs:59-66 | `connect` 未设 `application_name`，`pg_stat_activity` 里无法把写通道连接与其他客户端区分开 | `PgConnectOptions` 设 `application_name("dms-ai-owned")` | safe |
| owned.rs:61 | `max_connections(max_conn)` 无 `.max(1)` 下限；mysql.rs:448 已有同款钳制先例，`max_conn=0` 配置两侧行为不一 | 与 MySQL 侧对齐钳到 ≥1 | test |
| owned.rs:86 | `create_upload_table` 内含 `create_upload_schema`，而 tabular.rs:97 循环前已调过一次——每次上传多一次幂等但无谓的 `CREATE SCHEMA` 往返 | 两处留一个入口（建议收进 `create_upload_table`） | test |
| owned.rs:87-90 | DDL 失败时错误只有 `[owned-pg]`+PG 原文，不带「哪一步（schema/table/comment/grant）、哪张表」，半可用排查难 | 各步 `map_err` 附加步骤与表名上下文 | test |
| owned.rs:88-90 | 每列一条 `COMMENT ON` = 每列一次网络往返，百列上传表建表被 RT 放大 | 拼成一条 multi-statement（simple protocol）一次 `execute` | test |
| owned.rs:91 + tabular.rs:99-101 | 每张表调一次 `create_upload_table` → 同 schema 的 `grant_readonly`（角色存在查询 + 2 条 GRANT）每表重复一次 | 授权上移到 schema 级只做一次 | test |
| owned.rs:112-113 | GRANT 里 `{RO_ROLE}` 不加双引号，正确性依赖常量恰好全小写；未来改常量引入大写即静默失配 | 注释写明约束，或经 `SafeIdent` 渲染角色名 | safe |
| owned.rs:131 | 零列 spec 静默 `Ok(0)`——这是编程错误而非正常输入，静默成功掩盖调用方 bug | 改 `Err(ConnectorError::config(...))` 或至少 `tracing::warn!` | test |
| owned.rs:121-125 | 批间无事务：中途失败留下前 N 批残留，靠调用方 drop schema 兜底（kb_api），但 doc 注释没写这个约定 | `insert_upload_rows` 文档补「部分批次残留由调用方清理」 | safe |
| owned.rs:147 | `written as usize` 是 u64→usize 隐式转换（32 位平台截断；本仓虽只跑 64 位，属零成本防御） | `usize::try_from(written).unwrap_or(usize::MAX)` | safe |
| owned.rs:170 | `cell()` 只滤 `is_empty()`：空白串 `" "` 原样进 numeric/timestamptz 列触发 cast 错误；而 `infer_col_type`（ddl.rs:120）先 trim 再判空——推断与落库两侧空值口径不一致 | filter 改 `!s.trim().is_empty()` | test |
| owned.rs:185-206 | `render_insert_unnest` 用 4 个平行 `Vec` + 4 次 `join`；单循环直接拼一个 `String` 更直白、少 4 次分配 | 改为一次循环写 `String`（行为逐字节不变，现有单测直接守） | safe |
| owned.rs:158-164 | `ddl()` 成功路径零日志：建表/授权/删 schema 全静默，上传链路出问题只能等错误 | 加 `tracing::debug!`（schema/table 入字段） | safe |
| owned.rs:216-229 | 单测只覆盖 Numeric+Timestamptz 两列；列错位（casts/alias 编号不齐）无断言守 | 加一个三列（含 Text）用例断言 casts 与 alias 一一对齐 | test |

## review.rs（15 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| review.rs:39 | `fast()` 里 `.ok()?` 吞错且 None content 也静默——三类复核的 LLM 失败全链路零日志，自进化回路停转时无人知晓 | `fast` 内 Err/None 分支补 `tracing::debug!`/`warn!` | safe |
| review.rs:57 | `save_lesson_candidate(...).await;` 返回 `bool`（exemplar.rs:308）被直接丢弃——落库失败零痕迹，与 92-95 行 `review_exemplar` 的 warn 不对等 | 检查返回值，false 时 `tracing::warn!` | safe |
| review.rs:68 | LLM 挂掉时 `continue` 让 limit 条逐条各烧一次必败的 fast 调用（各付一次超时延迟） | 连续失败 ≥3 次即 break，本轮剩余下批再议 | test |
| review.rs:69 | 单条 `set_lesson_status` 写失败 `?` 上抛：整批中止、已复核成功的计数 `n` 随 Err 丢失；而兄弟 `review_all_pending`（102-113）逐条容错——两函数容错策略不一致，无注释说明为何 | 要么逐条容错+warn，要么注释钉住「整批原子」是有意的 | test |
| review.rs:94 | warn 把整句用户问题塞进结构化字段 `question`，长度不受控，日志行可膨胀到 KB 级 | 用仓里已有的 clip 手法截到定长 | safe |
| review.rs:23 与 144 | prompt 说「≤80字」、闸门是 `lesson.len() > 200` **字节**：80 个汉字 = 240 字节，按 prompt 合规的教训会被闸丢掉——两处阈值互相矛盾 | 对齐：`lesson.chars().count() > 80` 或把 prompt 改成「≤60字」 | test |
| review.rs:143 | `parse_lesson` 只认首行 `lesson=`（`strip_prefix`），模型多印一行前言就整篇丢弃；而 `parse_verdict`/`parse_opinion` 用 `contains`——三个 parser 三种宽严，无注释说明 | 逐行找第一个 `lesson=` 前缀行；或在注释里钉住严格是有意的 | test |
| review.rs:154 | `contains("verdict=enabled")` 命中否定语境（「不应判 verdict=enabled，应判 verdict=disabled」）→ 方向判反，且 enabled 是**宽松侧**（坏教训进后续所有 prompt） | 定位首个 `verdict=` 取其值精确比较 | test |
| review.rs:162 | `to_uppercase().contains("NEGATIVE")` 命中「not NEGATIVE」「opinion=POSITIVE 而非 NEGATIVE」→ 误剔；且每次调用一次全串大写分配 | 定位 `opinion=` 取值后 `eq_ignore_ascii_case` | test |
| review.rs:186-194 | `dead_pool` 文档注释两段开头重讲同一件事（「『PG 抖一下』的假件…」然后「『PG 写不进去』的假件…」），明显是两次编辑残留 | 合并成一段 | safe |
| review.rs:66 | 解构名 `trig` 语义不明（实际是 trigger_tables 的拼串），读者要跳到 exemplar 侧才知道 | 改名 `tables`/`trigger_tables` | safe |
| review.rs:109-113 | `review_all_pending` 逐条串行 await：limit=100 时 100 次 LLM RTT 串行，定时任务窗口内可能跑不完 | `buffer_unordered(4)` 之类小并发（改变负载形态，需评估 LLM 限流） | test |
| review.rs:38 | `Some(0.1)` 温度魔法数跨文件重复（同 insight.rs:236 / compound.rs:130） | 共享常量 | safe |
| review.rs:52,89 | 复盘/初筛 prompt 把用户问题、SQL、MySQL 错误原文（可能含数据值）原样进 prompt，全模块无 `wrap_untrusted`——与 insight/compound 的 I5 纪律不一致（模块文档 6-7 行声明逐字保留，故改动需过自进化口径评审） | 评估后给 user 段加包裹，或注释钉住「离线批处理、不防注入」的立场 | test |
| review.rs:53-54 | `review_failure` 的两个 early-return（fast 失败 / parse 失败）都静默——加上 57 行的吞 bool，整个函数失败路径零可观测性 | 各补一行 `tracing::debug!` | safe |

## pitfall.rs（15 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| pitfall.rs:21-22 | SQL 无 `ORDER BY`，`.take(cx.limit)` 的截断集随 PG 物理行序漂——与 metric.rs:78-81 已修过的确定性账同类；`meta.pitfall` 有 `id bigserial`（ddl.rs:94）可用 | `ORDER BY id`（改 prompt 字节序，需回归） | test |
| pitfall.rs:31 | 分隔符集 `[',', '，', ' | ']` 不含 `;`/`；`/`、`/空格：写错分隔符的 trigger 静默永不命中且零日志 | 补分隔符集，或对拆分后整串无命中的行 debug 留痕 |
| pitfall.rs:36 | `w.split('.').next().unwrap_or(w)`：`str::split` 恒产出 ≥1 项，`unwrap_or` 是死分支 | 去死码或注释说明「split 恒非空」 | safe |
| pitfall.rs:37 | `t == table_part` 大小写敏感；仓内测试自己认定 meta 里表名大小写两种写法都有（cards.rs:525）→ 大写 trigger 静默不命中 | `eq_ignore_ascii_case` | test |
| pitfall.rs:37 | 无 ≥2 字门槛：单字 trigger（如「退」）`contains` 必中一切相关问句，与 map_filter R1「中文单字无区分度」（kernel/nl/text.rs:45）纪律不一 | `w.chars().count() >= 2` 门槛 | test |
| pitfall.rs:36-37 | 「库.表.列」形态 trigger 的 `table_part` 取到的是库名 → 永不命中；ods.rs:46-51 已确立「裸名+限定名两形态都喂」标准，这里只认裸名 | 表名部分同时比对裸名/限定名两种形态 | test |
| pitfall.rs:40 | 多条 trigger 行命中同一 lesson 文本时不去重 → 同一句教训重复进 prompt | 按 lesson 文本 dedup（改 prompt 字节） | test |
| pitfall.rs:40 | 空串 lesson（ddl.rs:97 `lesson text NOT NULL` 不拒 `''`）原样收集，下游拼出空行 | filter 掉 `lesson.trim().is_empty()` | test |
| pitfall.rs:41 | `take(limit)` 截断无观测：命中 12 取 6 时无计数，对比 cards.rs:447 放宽路径有 info 留痕，调参无据 | 加 `tracing::debug!(hits, taken, …)` | safe |
| pitfall.rs:20,30,40 | `Vec<(String, String)>` 匿名元组 + ` | (_, lesson) | ` 解构，字段含义靠位置记 |
| pitfall.rs:22 | kind 清单 `('pitfall','routing','column_fix')` 硬编码无常量无注释：新增 kind 不进召回，也无测试钉这份清单 | 提常量 + 注释「新增 kind 需同步」 | safe |
| pitfall.rs:19-43 | 命中判据是纯字符串逻辑却焊在 async DB 函数里，全文件零单测；仓内同族都把判据拆纯函数（cards.rs:217 `value_hint_cards`、ods.rs:63 `apply_boost`） | 抽 `fn trigger_matches(question, tables, trig) -> bool` 纯函数并补无库单测（行为不变） | safe |
| pitfall.rs:37 | 判据顺序 `question.contains(w) |  | tables.any(...)`：对「表名.列名」形态 contains 几乎恒 false，便宜的表名全等比较排在贵的子串扫后面；两判据无副作用，交换零行为差 |
| pitfall.rs:20-27 | 每问句 `format!` 重建同一 SQL 文本（`ds_pred(1)` 产物对固定参数是确定串） | OnceLock 缓存 SQL 文本（全部召回函数同族，一次修） | safe |
| pitfall.rs:5-7 vs 17-18 | 模块 doc 与 fn doc 把「trigger 锚到会被检索到的表名上」同一语义重复表述两遍 | 合一，fn doc 只留补充 | safe |

## crates/semantic/src/ddl.rs（15 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/semantic/src/ddl.rs:417-418 | 注释「不在表里的三张」实际列了四张（pitfall/sql_exemplar/scope_binding/datasource），注释与代码不符 | 改「四张」 | safe |
| crates/semantic/src/ddl.rs:444 | `d.contains("ds_id")` 只验「包含」不验「前置」：PK 为 `(table_name, ds_id)` 会被误判已迁移，与本文件「前置」语义及 `ds_pk_is_prefixed`（L582-589）的钉法不一致 | 改为校验 def 以 `PRIMARY KEY (ds_id` 开头，并补一条单测 | test |
| crates/semantic/src/ddl.rs:447 | `DROP CONSTRAINT IF EXISTS {t}_pkey` 写死默认约束名；若某表 PK 被手工建过（非默认名），DROP 空转、下一步 ADD 报 "already exists" 且文案不指因 | 用 L437-443 查到的 conname 去 DROP | test |
| crates/semantic/src/ddl.rs:447-450 | DROP CONSTRAINT 与 ADD PRIMARY KEY 两条独立语句无事务：进程在两者之间崩溃 = 该表无 PK 直到下次启动成功 | 两句包进一个事务 | test |
| crates/semantic/src/ddl.rs:436-443 | 每次启动对 10 张表逐张查 `pg_constraint`（10 次 round trip） | 一次 `WHERE conrelid = ANY($1)` 全查回，Rust 侧比对 | safe |
| crates/semantic/src/ddl.rs:409-411 | 逐句执行失败时 anyhow 错误不含是第几句（sqlx 默认不带 SQL 文本），60+ 句里定位靠猜 | `.map_err` 附上截断到 80 字的 stmt 文本 | safe |
| crates/semantic/src/ddl.rs:125-130 | artifact 版本回填 UPDATE 每次启动都跑：即便 WHERE 恒 no-op，子查询仍全表扫 + window 计算一遍 | 用 meta.kv 记「已回填」哨兵跳过一次跑过的迁移 | test |
| crates/semantic/src/ddl.rs:118 | `idx_artifact_share` 索引含全部空串行（未分享产物占绝大多数），而 L116 注释自认查询只认 `share_token <> ''`——空串白占索引 | 改部分索引 `WHERE share_token <> ''`（注意 CREATE INDEX IF NOT EXISTS 不会替换旧索引，老库要 DROP 一次） | test |
| crates/semantic/src/ddl.rs:339-340 | `idx_correction_trace`/`idx_failure_trace` 含 `trace_id IS NULL` 的老行，同理白占 | 部分索引 `WHERE trace_id IS NOT NULL` | test |
| crates/semantic/src/ddl.rs:477-479+496 | 三个 bool 按位置解码成 `(bool,bool,bool)`：SQL 列序一旦被调整，三个就绪位静默张冠李戴（类型全同，编译期抓不到） | SQL 里给三列起别名，`sqlx::query` + `Row::get` 按名取 | safe |
| crates/semantic/src/ddl.rs:38-40,409 | 「注释里不许带半角分号」只有注释自述、无任何自动化判据；再踩一次就是服务与全部 CLI 启动即语法错误 | 加无库单测：DDL 中 `--` 注释行不含 `;` | test |
| crates/semantic/src/ddl.rs:42-50+389 | 全新空库上 table_doc 先建 `PK(table_name)`、再 ALTER 加 ds_id、再 rekey DROP/ADD——新库白做一次主键索引重建；CREATE 里直接带 ds_id 列+复合 PK 对老库无害（CREATE IF NOT EXISTS 对老库不生效，老库走 ALTER 路径） | CREATE 语句直接声明 `(ds_id, table_name)` 主键 | test |
| crates/semantic/src/ddl.rs:408-414 | migrate 本体成功路径零日志（rekey 有 info!，建表建索引没有），启动排障时看不到「N 句已执行」 | 收尾 `tracing::debug!`/`info!` 一句带语句数 | safe |
| crates/semantic/src/ddl.rs:585 | `ds_pk_is_prefixed` 只钉 DS_PKS 里的 10 张；value_domain/table_snapshot/document_family「建表即带 ds_id 主键」没有任何判据，谁把它们的 PK 顺序改坏无人发现 | 测试追加断言这三张的内联 PRIMARY KEY 以 ds_id 开头 | safe |
| crates/semantic/src/ddl.rs:273-276 | 注释里硬编码「现有九种」kind 清单，guard 侧加第 10 种时这里必漂 | 注释改为指向 kind 的产出方而非枚举计数 | safe |

## qa_log.rs（15 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| qa_log.rs:61,91 | `elapsed_ms` 入参用 `u128`（仅为对齐 `Instant::elapsed().as_millis()` 的返回型），Entry 内立即 clamp 回 i64；签名把内部精度泄漏给调用方 | 参数直接收 `u64`（毫秒在 u64 内永不溢出），Entry 里 `min(i64::MAX as u64)` | safe |
| qa_log.rs:83 | `citations_of(a) as i32` 裸 as 转换：usize→i32 理论可回绕；同函数 92/93/96 行对 tokens/llm_calls 都做了 `min(i32::MAX)` clamp，独漏 row_count——同函数两套防御标准 | `citations_of(a).min(i32::MAX as usize) as i32` | safe |
| qa_log.rs:84,108 | 失败路径 `e.to_string()` 被求值两次（entry 里 sanitize 一次、status_of 里 timeout_marked 又一次），错误文案越长浪费越明显 | `status_of` 改收预算的 `&str`（entry 内先算 `let msg = e.to_string()` 复用） | safe |
| qa_log.rs:66-70 | spawn 的 fire-and-forget 在进程 shutdown 时会静默丢行（tokio 不保证 detached task 跑完），目前只在「insert 失败」时 warn，「任务被 runtime 丢弃」连 warn 都没有 | 可接受但应在 53-54 行文档注释里写明这个丢失窗口（现在只写了「失败只 warn」） | safe |
| qa_log.rs:68 | warn 直接打 `{err}`：ConnectorError 原文可能带回 SQL 片段/绑定值（含用户问题原文），而落库路径 84 行专门做了 sanitize——日志面反而没脱敏 | `qalog::sanitize(&err.to_string())`（必要时再 clip）后打日志 | safe |
| qa_log.rs:116 | `let AnswerBody::Text {...} else { return String::new() }`：KB 永远只产 Text，此分支是死代码；若未来新增 body 变体，这里静默吞掉摘要（sql 空串=succeeded 失败行难分辨） | else 分支加 `debug_assert!(false, ...)` 或 `tracing::warn!` 一次 | safe |
| qa_log.rs:123-124 | 先截 60 字再去重：两篇不同文档若前 60 字相同会被并成一篇（去重键是截断后的名字），摘要少报 | 先按全名去重再截断展示；或注释说明接受此近似 | test |
| qa_log.rs:129 | 文案「引用{}篇」填的是 `citations.len()`（引用**条数**，同一文档多 chunk 会重复计），括号里列的却是去重后的文档名——「引用3篇（A、B）」这种数与名对不上的运营文案 | 改用 `names.len()`（真实篇数），或文案改「引用{}处」与条数对齐 | test |
| qa_log.rs:131 | 「等{}篇」用 `names.len()`（唯一文档数），与 129 行 `citations.len()` 混用两种计数口径于同一句话 | 与 129 行统一口径（同一条修法） | test |
| qa_log.rs:133 | `qalog::clip(&s)` 最后兜底截断可能把「等N篇」后缀或文档名从中间砍掉，读者无从察觉被截 | clip 前先判断长度，超长时优先砍名单尾部并保留「 等N篇」 | test |
| qa_log.rs:143 | `insert` 返回 `Result<u64,...>`，调用方（67 行）只看 Err，rows_affected 恒被丢；写 0 行（异常但非错误）不会被察觉 | 调用处 `Ok(0) => tracing::warn!("落账 0 行")`，或删返回值 | safe |
| qa_log.rs:161 | `.bind(Some(&e.trace_id))` 无防空：注释承诺「空串才落 NULL、trace_id 恒有值」，但函数是 pub，外部传 `""` 会落 `''` 而非 NULL，破坏与 server 的同约定 | `Some(&e.trace_id).filter( | s |
| qa_log.rs:92-93,96 | 三处 `x.min(i32::MAX as u32) as i32` 同一 pattern 重复，若 Usage 字段类型变更要同步改三处 | 抽 `fn clamp_i32(v: u32) -> i32` | safe |
| qa_log.rs:174-196 | 测试夹具 `citation()` 13 个 None/空字段手工铺，Citation 加字段时编译错点分散；与 answer.rs:989 的 `hit()` 夹具同病（两个文件各自维护一份样板） | 若 kernel 提供 `Citation::default()`/builder 则复用；否则维持但抽 `..citation("")` 式复用已在用——本条仅建议加注释指向 answer.rs 夹具保持同步 | safe |
| qa_log.rs:207-209,268-278 | 两组源锚点测试用 `include_str!` + 字符串查找钉契约，重构改名（如 `qa_log::finish` → 别的名字）时会以「找不到字符串」的方式红，报错文案不指向真实契约 | 已带 expect 文案，基本够；可在 expect 里补一句「若是改名请同步改本锚点」 | safe |

## dms_lookup.rs（15 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| dms_lookup.rs:26-30,453 | doc 说「覆盖凭据、手机号…字段」，实现是**子串**匹配（`contains`）：`tokenizer_ver`（含 token）、`phone_ext` 这类列一并拒——多拒方向未在 doc 写明 | doc 补一句「子串匹配，宁宽勿漏」 | safe |
| dms_lookup.rs:39-48,58-75 | 策略表与登记键表是两份平行常量，漂移无编译期防线（今天人工核对一致） | 加测试断言：SCOPED_LOOKUP_POLICIES 的每个 (table,col) 必在 REGISTERED_LOOKUP_KEYS | test |
| dms_lookup.rs:102 | `new` 的形参名 `unique_cols` 与字段名 `lookup_cols` 不一致 | 统一为 `lookup_cols` | safe |
| dms_lookup.rs:172-174,296 | 表名被提取两次：`validate_query` 内 `query_table_ref`（296）+ 出口处 `query_table_name`（174）再走一遍 | 让 `validate_query` 顺带返回 actual_table | safe |
| dms_lookup.rs:192-195 | table 已 `to_ascii_lowercase`，195 行仍 `eq_ignore_ascii_case` | 改 `==`（policy.table 全小写常量） | safe |
| dms_lookup.rs:212-213 | `is_safe_select_with` 内部已 parse 一次，213 行又 parse 一次 | 透出 AST 复用（同 guard 三次 parse 条） | test |
| dms_lookup.rs:260-263 | `LIMIT 99999999999999999999` parse 溢出 → 报「LIMIT 必须是整数常量」，文案误导（它是整数，只是超 usize） | 区分「非整数」与「超出可表示范围」两文案 | safe |
| dms_lookup.rs:229-240 | `normalize_limit` 对已验证过的 limit 再 parse 一次；validate 已保证 ≤50，`.min()` 是双保险（可留，但注释未说） | 注释一句「clamp 是纵深第二道」 | safe |
| dms_lookup.rs:383 | `unreachable!()` 紧跟在 369-380 的 matches! 检查之后，双匹配写法易在改 pattern 时失真 | 改为单次 `let TableFactor::Table{...} = relation else { return Err(...) }` | safe |
| dms_lookup.rs:459 | `fun.name.to_string().to_ascii_lowercase()` 两次分配 | `to_string_lowercase` 或先取 last 段再 lower | safe |
| dms_lookup.rs:471 | `rsplit('.').next().unwrap_or(name)`：rsplit 恒非空，unwrap_or 是死分支 | 删 unwrap_or（`.next()` 直接用） | safe |
| dms_lookup.rs:520,581-589 | `deleted_flag = '0'`（字符串形态）落入「WHERE 条件列必须是登记的索引键」文案，用户写的恰恰是被允许的 deleted_flag，文案误导 | soft-delete 判定接受 SingleQuotedString("0")，或文案补「（数值 0）」 | test |
| dms_lookup.rs:552 | 登记键只认 `SingleQuotedString`；MySQL 默认下 `"SO-1"` 也是字符串，会被拒（安全方向，但与 MySQL 语义不符且未注释） | 注释声明「只认单引号」或放行 DoubleQuotedString | test |
| dms_lookup.rs:259-267 | limit 校验逻辑（Value::Number + parse）与 normalize_limit(233-238) 是同一段代码的两份拷贝 | 抽 `fn const_limit_usize(e) -> Option<usize>` 共用 | safe |
| dms_lookup.rs:174,192 | `to_ascii_lowercase()` 在已全小写输入上仍分配 | `if s.bytes().any( | b |

## doc.rs（15 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| doc.rs:110 | `expect("doc http client")` 同 embed.rs:62，未注释取舍 | 注释或 `new` 返 `Result` | safe |
| doc.rs:89 | TooLarge 文案硬编码「20 万行 / 200 列」，与 Python 侧上限（tools/embed_service.py）双写漂移 | 文案泛化为「表格超出服务端上限」或注释互指 | safe |
| doc.rs:148 | `health()` 不看 HTTP 状态码：服务 500 带 JSON body 也当健康透传进 `/api/health` | `error_for_status()` 或显式检查 status | test |
| doc.rs:147-148 | health 两级 `.ok()` 吞错：「服务不可达」与「回了坏 JSON」在 /api/health 里无从区分 | 加 `tracing::debug!` 留痕 | safe |
| doc.rs:160 | 每请求 `format!("{}{path}", self.base)` 分配 | `new` 时预拼 `/parse`、`/chunk`、`/health` 三个全 URL 字段 | safe |
| doc.rs:172 vs 177 | 只有 send 失败进熔断；177 行 body 读取失败（连接中途断）同属网络类却不熔断，与 171 行注释「网络类失败：熔断 300s」口径不符 | 177 的 Transport 也熔断，或收窄注释为「连接/发送失败」 | test |
| doc.rs:177 | `resp.text().await` 无大小上限：异常服务可回超大 body 撑内存，且错误 body 原样进 `DocError::Api` 上抛给用户/日志 | body 截断（如 4KB）再进错误变体 | test |
| doc.rs:182 | JSON 解析失败塞进 `Transport` 变体，Display 出来是「文档服务不可达：响应不是预期 JSON」——自相矛盾的误导文案（明明可达） | 新增 `BadResponse(String)` 变体，或 Transport 文案泛化 | test |
| doc.rs:190-206 | `api_error` 用 `body.contains(...)` 子串匹配定确定性失败：`"unsupported"` 会命中任意含该词的自由文本，把服务端 bug 型 422 误判成「换文件才有意义」；多关键词同时出现时优先级靠 if 顺序隐式决定 | 按 187-188 行注释的契约解析 JSON 的 `error` 字段精确匹配 | test |
| doc.rs:116 | `mime.unwrap_or("")`：空串与 None 同义是隐式 wire 契约，无注释 | 一行注释（wire 形状不动） | safe |
| doc.rs:64-78 | 调用方要自行 match 变体区分确定性/网络性失败（187-189 行描述的 KbError 映射靠手写 match） | 加 `pub fn is_deterministic(&self) -> bool`（纯新增 API） | safe |
| doc.rs:106-112 | 与 embed.rs:58-65 的构造逻辑（trim_end_matches、client 构建、cooldown 字段）逐字同构，无互指注释 | 互指注释或抽公共（跨 struct 不值当，注释即可） | safe |
| doc.rs:161-177 | `post` 无耗时留痕：120s 预算的 parse 跑 60s 完全不可见；fixed.rs:162/184 已有「偏慢 warn」先例 | 记录 started，超阈值 `tracing::warn!` | safe |
| doc.rs:140-149 | `health()` 不走 `post`、不查熔断——穿透是刻意的（健康检查本就该探），但无注释说明这个不对称 | 一行注释 | safe |
| doc.rs:220-235 | 测试缺口：`api_error` 的 404→NotFound、422→TooLarge 两分支无断言（只测了 no_text_layer/unsupported/500/boom） | 补两个 `matches!` 断言 | safe |

## registry.rs（15 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| registry.rs:54-56 | `preload` 对同 ds_id 静默覆盖旧池，无日志无断言 | 覆盖时 `tracing::debug!` | safe |
| registry.rs:74-76 | 缓存命中不校验 spec（dsn_ref/max_conn/schema 变了仍复用旧池，须先 `close` 才重建）——隐含契约未写 | 在 `get` 文档注释写明「改配置必须先 close」 | safe |
| registry.rs:88-115 | `probe` 连接无超时：黑洞主机让「测试连接」按钮永远转圈 | 给 connect/query 包 `tokio::time::timeout` | test |
| registry.rs:103-113 | PG 的 probe 忽略 `spec.schema`：schema 配错也能测通，之后 schema 采集才空手（29-31 注释自认必须给） | probe 里校验 schema 存在性 | test |
| registry.rs:119 | `pub async fn close` 函数体内零 await，无谓 async | 改同步 fn（调用点去 `.await`） | test |
| registry.rs:154-158 | dsn_ref 未配置的报错不带已配置键数，排障要多翻一遍配置 | 文案附 `self.dsns.len()` | safe |
| registry.rs:161-163 | 注释称「中毒只可能来自持锁时 panic，而持锁段只有 HashMap 操作」=中毒不可能，却又写了恢复分支——注释与防御代码互相矛盾 | 二选一：改 `.expect` 或修正注释 | safe |
| registry.rs:62,80 vs 160-163 | `policies.lock().unwrap_or_else(...)` 写两遍，pools 却有 `lock()` helper | 抽 `policies_lock()` 对齐 | safe |
| registry.rs:170-171 | 无 `://` 的 `user:pass@host` 形态不进 userinfo 遮蔽分支（仅 `password=` 键会被 mask） | 注释写明覆盖边界 | safe |
| registry.rs:191 | `split_once('=').unwrap_or((tail,""))` 是死分支（find_secret_key 保证有 `=`） | 改 `expect`/`debug_assert` 明示不变量 | safe |
| registry.rs:195 | 值终止符只有 `&`/` `，ADO 风格 `Password=x;User=…` 的分号不认（结果是把剩余参数全吃掉，安全方向但日志信息丢失） | 终止符集加 `;` | safe |
| registry.rs:204-211 | `pwd=` 不要求键边界，`expwd=x`、路径 `/expwd=1` 会被误遮（过度遮蔽） | 要求前导为起始/`&`/`?`/空格/`;` | test |
| registry.rs:207 | 每次调用整串 `to_ascii_lowercase()` 分配，`mask_params` 循环里 O(n·k) | 单趟扫描复用一份小写 | safe |
| registry.rs:45-51 | `new(dsns)` 接受空映射不吭声——启动零 DSN 多半是配置事故 | 构造时 `tracing::debug!` 键数 | safe |
| registry.rs:127 | 建池日志只有 ds_id + 脱敏 DSN，缺 kind/max_conn | 日志补 kind/max_conn | safe |

## external_kb.rs（15 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| external_kb.rs:48-53 | `now()` 与 rerank.rs:25-30 重复 | 共享（见 rerank 条目） | safe |
| external_kb.rs:17-20 | 常量与 rerank.rs:12-15 重复 | 共享 | safe |
| external_kb.rs:58 | dataset 直接拼 URL 不编码：含 `/`/`?`/`#` 即破路径 | 校验字符集或 percent-encode | test |
| external_kb.rs:57-65 | 直接调 `new` 无空值校验（from_vars 才有） | `debug_assert!` 非空 | safe |
| external_kb.rs:88-91 | top_k 无上限原样透传 | clamp | test |
| external_kb.rs:96-100 | 每次调用新建 Client（ponytail），丢连接复用 | 结构体缓存 Client | test |
| external_kb.rs:100 | Client build 失败 `.ok()?` 静默 | debug 日志 | safe |
| external_kb.rs:118 | 不查 `resp.status()`：401 与形状不符不可区分 | 非 2xx 时 warn 带状态码 | safe |
| external_kb.rs:122-126 | send 失败无日志 | `tracing::debug!` 带 error | safe |
| external_kb.rs:136-166 | 丢记录（无 id/空正文）不计数不留痕 | debug 日志丢弃条数 | safe |
| external_kb.rs:148 vs 149 | `document_id` 不 trim、`name` trim——`document_id=" "` 时 152 的 `!is_empty()` 成立，document_name 落成空白串 | document_id 同样 trim | test |
| external_kb.rs:163 | source_uri 内嵌未编码的 segment_id（今天都是 UUID，但无强制） | 注释说明前提或编码 | safe |
| external_kb.rs:35 | `score: f64` 与 rerank 侧 f32 口径不一 | 统一（公开类型变更） | test |
| external_kb.rs:88-89 | query trim 后无长度上限，超大问句原样外发 | clamp 或注释契约 | test |
| external_kb.rs:40-46 | 无脱敏 `Debug`（同 rerank） | 手写脱敏 `Debug` | safe |

## config/index.js（14 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| config/index.js:1 | `{isDevModel}` 无空格、双引号，与仓库主流单引号+空格风格不一 | 格式化统一 | safe |
| config/index.js:2,4,12 | 缺分号，与 L21 有分号不一致 | 统一补分号 | safe |
| config/index.js:4 | `` `${envBaseUrl}` `` 多余模板字符串，envBaseUrl 已是 string | 直接 `let baseUrl = envBaseUrl` | safe |
| config/index.js:2,4 | L2 ` |  | ""` 与 L4 模板包裹是两步冗余，可合并 |
| config/index.js:7 | 三元 `?baseUrl :"[baseUrl]"` 缺空格、缺括号，优先级靠记忆 | 加空格并括号包裹条件 | safe |
| config/index.js:7 | 条件 `isDevModel() |  | envBaseUrl` 语义模糊（env 非空时 dev 判断被短路），无注释解释为何生产兜底占位符 |
| config/index.js:7 | 占位符 `"[baseUrl]"` 若误入生产包，请求静默打到相对路径无任何告警 | 该分支加 `console.warn('[config] baseUrl 未配置')` | safe |
| config/index.js:12 | 默认值硬编码 HTTP IP，提交在源码里；生产必须 HTTPS（L11 注释已述）但仍提供不安全兜底 | 默认改空串 + 启动时 warn；或至少注释说明该 IP 仅开发用 | test |
| config/index.js:11 | 注释未提开发期需在开发者工具勾选「不校验合法域名」，新人真机/模拟器调试会踩坑 | 注释补一句 | safe |
| config/index.js:15 | `version:'1.9.0'` 冒号后缺空格 | 格式化 | safe |
| config/index.js:15 | version 全仓唯一来源靠手改，发版易漏 | 注释提醒发版 checklist 或由构建注入 | safe |
| config/index.js:18 | `timeout:60*1000` 无注释说明单位/适用面（AI 问答同样吃这 60s） | 补注释 | safe |
| config/index.js:21 | 行尾多余空格 | 去掉 | safe |
| config/index.js:14-19 | 对象字面量键值无空格对齐，与仓库其它配置对象风格不一 | 统一格式化 | safe |

## tools/cli.py（14 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| tools/cli.py:37 vs 49 | `_drop_stdin_flag` 用 `Path(base[0]).stem == "docker"`（区分大小写），`_ensure_stdin_flag` 用 `.stem.lower()`——`DMSAI_CLI="Docker exec -i ..."` 时剥旗失效、补旗生效，两处口径不一 | 行 37 也加 `.lower()`，并在 `__main__` 自检补一条大写断言 | test |
| tools/cli.py:37 vs 52 | `_drop_stdin_flag` 只认 `base[1:3]` 里的 exec，`docker --context prod exec -i c ...`（exec 在下标 3）剥不掉 `-i` → 正是文件头发誓要防的挂死；`_ensure_stdin_flag` 却用 `base.index("exec",1)` 任意位——剥/补不对称 | 行 37 改为同样的 `index("exec", 1)` 探测，自检补 `--context` 用例 | test |
| tools/cli.py:38 | 列表推导剥掉 base 里**所有** `-i`，包括容器内命令自己的（`docker exec -i c python -i` → 两个都被剥）；`_ensure_stdin_flag` 的 docstring 特意防了这个误判，剥的一侧却没防 | 只剥 exec 与容器名之间的 option 段，自检补断言 | test |
| tools/cli.py:97 | `cli()` 每次调用都 `stale_exe()` → `root.rglob("*.rs")` 全树扫一遍；一轮回归 55+ 次调用 = 55+ 次目录扫描，且会钻进 `crates/target/` | 对 `stale_exe()` 结果做模块级缓存；rglob 时跳过 target 目录 | safe |
| tools/cli.py:98/106 | `exe.stat()` 调了两次 | 存一次复用 | safe |
| tools/cli.py:111-117 vs 126-132 | `cli()` 与 `cli_stdin()` 的环境读取 + stale 校验 + shlex 构造段完全重复，只差剥/补旗一步 | 抽 `_base_argv()` 私有助手，两函数各调一次 | safe |
| tools/cli.py:116/131 | `shlex.split(pre)` 默认 posix 模式会吃掉 Windows 反斜杠：`DMSAI_CLI=C:\tools\wrap.exe` → `C:toolswrap.exe`，静默拼出不存在的路径 | 头注写明「Windows 路径需引号或正斜杠」，或失败时给出可达性检查提示 | safe |
| tools/cli.py:135-137 | `available()` 只看 exe 存在/环境变量，不看是否过期；但 `cli()` 对过期 exe 是 SystemExit 硬失败——调用方按 available()=True 走然后整进程被炸掉，「依赖缺席跳过」语义被穿透 | available() 里同步 `stale_exe() is None` 判据，或 docstring 写明差异 | test |
| tools/cli.py:27 | 只认 `target/debug/...exe`；只编了 release 的机器上 available()=False、`cli()` 回落到不存在的路径 | docstring 注明「只看 debug」是刻意的，或加 release 回落 | safe |
| tools/cli.py:8 vs 105 | 头注示例用 cmd 语法 `set DMSAI_CLI=...`，stale_exe 报错文案用 PowerShell 语法 `$env:DMSAI_CLI=...`——同文件两种 shell，用户照抄必有一处不通 | 头注两种 shell 各给一行，或与行 105 统一 PowerShell | safe |
| tools/cli.py:15 | 头注说「`-i` 从来没被需要过」，但行 19-20 又说 `eval-batch` 必须走 stdin——两句连读自相矛盾（应限定为「一次性子命令不需要」） | 行 15 改为「一次性子命令从没需要过 `-i`」 | safe |
| tools/cli.py:178-179 | 自检里 `assert stale_exe()` 依赖本机环境状态（SAC 是否拦链接）：哪天 SAC 松了、exe 真新了，自检永久红而代码没错——环境耦合的脆弱断言 | 降级为 print 警告，或注释里写明这是有意的水位计 | safe |
| tools/cli.py:52 | `base.index("exec", 1)`：若镜像/参数里先出现字面量 `exec`（如 `docker run exec ...`），会误判位置并在错误处插 `-i` | 找到 exec 后先校验它是子命令位（前面 tokens 都以 `-` 开头或是 docker 全局 option） | test |
| tools/cli.py:199 | 自检结束只打「cli.py 自检通过」，不像 regression.py:487 那样列出验了哪些判据，回归时无法核对覆盖面 | 打印覆盖清单（剥 -i / 补 -i / 容器内 -i 不误判 / 过期硬失败 / 回落） | safe |

## scripts/parser.ps1（14 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| scripts/parser.ps1:66-67 | L66 在判 HTTP 状态之前先 `$r.Content \ | ConvertFrom-Json`——错误体非 JSON 时 ConvertFrom-Json 抛错，$ErrorActionPreference=Stop 直接终止脚本，走不到 L69 记 bad、也走不到 L190 汇总退出码（L55-59 注释强调的那条纪律被自己的顺序绕过） | 把 L66 移进 200 分支内 |
| scripts/parser.ps1:73 | `$j.sheets` 为 $null 或空数组时 `ConvertTo-Json -Compress` 产出 `'null'`/`'[]'` 非空字符串 → 绕过 L74 零文本判红，「静默返空」漏判 | 判 `$j.sheets.Count -gt 0` 再序列化 | test |
| scripts/parser.ps1:64,89 | 连接被拒（容器没起）时 Invoke-WebRequest 直接抛错终止整个 probe，已过/未过格式无汇总、$script:bad 不结算 | 两个 Probe 函数加 try/catch，记 bad 后继续 | test |
| scripts/parser.ps1:68,92 | 错误分支原样打印 `$r.Content` 全文，长 HTML 错误页/大响应刷屏；parse_service.py:72 对上游错误体已截断 500 字符，探针侧没有 | 截断到 500 字符 | safe |
| scripts/parser.ps1:115 | 'down' 对不存在的容器也打「已停」，rm 失败被静默——文案与事实可能不符 | 先 `docker ps -a` 判断存在与否，分别打「已停」/「本就不在」 | safe |
| scripts/parser.ps1:134 | 健康窗口 30×700ms=21s，而 serve.ps1:95 给后端 63s；解析容器首启要 import fitz/pytesseract 等重依赖，慢机 21s 偏紧 | 放宽到 60×700ms | safe |
| scripts/parser.ps1:141 | 失败文案「health 30 次未通」同 serve.ps1:103 的次数/秒口径问题 | 统一用秒 | safe |
| scripts/parser.ps1:150-151,164-166 | 两次造夹具各起一次性容器，第二次仅多挂 `/mk`——可合并为一次调用，省一次容器冷启动（秒级） | 合并两条 docker run | safe |
| scripts/parser.ps1:188 | 上游判据失败时直接 exit 1，跳过 L190-194 的逐格式失败汇总——一趟看不到完整红单，注释（L189）说「在这里才结算」却被前面的 exit 截胡 | 先结算 $script:bad 再判上游退出码 | safe |
| scripts/parser.ps1:195 | 成功文案「5 种格式全部解析出文本」漏算 3 条 token 探针（L168-170）与上游判据——文案与实际执行内容不符 | 改「5 格式 + 3 token + 上游判据全绿」 | safe |
| scripts/parser.ps1:27,174 | `-Port` 非默认时 L174 注释自认上游 parse_probe.py 写死 127.0.0.1:8077——打错地方且无任何提示 | -Port ≠ 8078 时打警告，或给 parse_probe 传 BASE | safe |
| scripts/parser.ps1:27 | $Port 无范围校验，传 0/负数/超 65535 时错误出现在远端 docker 层，文案不直观 | `[ValidateRange(1,65535)]` | safe |
| scripts/parser.ps1:14 | 注释「切过去只改 settings.json 的 `service_url`」，而 serve.ps1:26 认的是 settings.docker.json——两处注释对「该改哪个文件」口径不一 | 统一为 settings.docker.json（容器部署语境） | safe |
| scripts/parser.ps1:49-51 | 注释「宿主机 embed 绑 127.0.0.1，需要它改绑 0.0.0.0（不在本轮范围，见 needs_from_others）」已过时：embed_service.py:1690 已有 host 参数，且「改绑 0.0.0.0」与 embed_service.py:1243「0.0.0.0 会把解析/向量面暴露给公网」直接矛盾 | 注释改为指向 host 参数与 172.17.0.1 推荐值 | safe |

## wework.rs（14 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| wework.rs:24-28,141-168 | `TOKEN` 缓存不按 corpid 区分：多企业配置时互相串 token（当前单配置未触发） | `TokenCache` 记录 corpid 并比对，miss 即重取 | test |
| wework.rs:38-43,150,178,205 | `http()` 每次新建 `reqwest::Client`（`login_by_code` 一次建 3 个），无连接复用 | `OnceLock<reqwest::Client>` 共享 | safe |
| wework.rs:129 | 非 2xx 时状态码与响应体全丢，只回「服务不可用」，排障无据 | 错误带 `resp.status()` | safe |
| wework.rs:157-159,185-187,212-214 | errcode≠0 时 bail 不带 errcode/errmsg；企微错误码（如 40014/42001）是排障关键 | bail 附 `errcode`/`errmsg` | safe |
| wework.rs:65 | `starts_with("http://localhost")` 前缀过宽：`http://localhost.evil.com`、`http://localhost@evil.com` 均通过校验，OAuth code 可被引到第三方 | 解析 URL 后校验 `host == "localhost"` | test |
| wework.rs:142,166 | `if let Ok(guard)` 锁中毒时静默放弃缓存：每次都打企微 gettoken，撞限频 | 中毒时 warn 或 `into_inner()` 恢复 | safe |
| wework.rs:136-169 | 缓存 miss 后无 single-flight：并发登录同时 gettoken（企微有频次限制） | miss 后拿锁再二次检查，或换 tokio Mutex 跨 await 持锁 | test |
| wework.rs:165 | `expires_in` 缺失默认 7200，且对 0/异常大值无防御 | clamp 到合理区间（如 60..=7200） | test |
| wework.rs:144 | 提前刷新余量 300 硬编码，注释「提前 5 分钟」与值双写 | 提常量 `REFRESH_AHEAD_SECS` | safe |
| wework.rs:234 | 文案「企微通讯录未返回手机号或姓名」误导：实际是「手机号未匹配到员工 且 无姓名可兜底」，mobile 明明返回了 | 改准文案 | safe |
| wework.rs:228-232 | 手机号命中多名员工时 bail「不唯一」但服务端无 warn，事后无法定位脏数据 | bail 前 `warn!`（不含完整手机号，可打码） | safe |
| wework.rs:45-56 | 手写百分号编码器；reqwest 传递依赖里已有 `url::form_urlencoded` | 复用或注释「刻意零新增依赖」 | safe |
| wework.rs:107-119 | `oauth_cookie`/`clear_oauth_cookie` 模板重复，Secure 拼接逻辑两处 | 抽内部 helper | safe |
| wework.rs:31-36 | `now()` 的 `unwrap_or(0)`：时钟异常时全量判过期（fail-closed，合理但无注释） | 加一行注释说明刻意 fail-closed | safe |

## ods.rs（14 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| ods.rs:6 | 「（ warn 留痕）」前导空格，排版脏 | 去空格 | safe |
| ods.rs:68-74 | `apply_boost` 比较器内 `boosted.contains` 每对比较算两次（O(n log n) 次哈希查） | `sort_by_key((Reverse(boosted.contains(t)), Reverse(score), t))`：每元素算一次，全序键行为全等 | safe |
| ods.rs:96-104 | `.flat_map( | table | { …; [Some(..), qualified] }).flatten()` 双重拍平：flat_map 直接返回数组即可 |
| ods.rs:96-104 | `forms` 不去重：调用方传重名表时 `ANY($1)` 数组白膨胀（无害但可省） | `forms.sort(); forms.dedup();` 或 HashSet 收集 | safe |
| ods.rs:105-114 | `UNION ALL` 两源：同一对表在 join_edge 与 datamap_edge 各有边时证据重复行进 prompt，本函数不去重 | SQL 侧 `UNION`（去重）或 Rust 侧 dedup——改 prompt 字节 | test |
| ods.rs:110 | `confidence >= 0.9` 魔法数；同文件 L20 的 `LINEAGE_ANCHORS` 都有常量+注释的待遇 | 提 `const JOIN_MIN_CONFIDENCE: f64 = 0.9` | safe |
| ods.rs:110-111,144 | `status <> 'rejected'`：status 为 NULL 的边被静默排除（`NULL<>'rejected'`=NULL）；若 DDL 是 NOT NULL 则无害，但前提未在本文件钉 | 注释钉 DDL NOT NULL 前提，或 `OR status IS NULL` | safe |
| ods.rs:126-133 | rows→JoinEvidenceRow 手排 4 元组 map | `#[derive(sqlx::FromRow)]` 直接 query_as 到 struct | safe |
| ods.rs:105-125 vs 138-159 | 「查询 → match Err → warn → 空集」模板整段重复两份（仓内同族还有更多） | 提小 helper（`query_or_warn_empty`） | safe |
| ods.rs:33-52 | `scored` 被遍历两遍（pool 取 detail_layer、anchors 取 !detail_layer，谓词互补）——一遍循环可同时产出 | 合成一次分区循环（纯可读，量小） | safe |
| ods.rs:26-59 | `ods_candidate_tables` 全程无日志：候选 0 张 / boosted 0 张 / 截断于 limit 均无据——与本仓自述「召回为什么是空是最高频排查题」（schema.rs:90-94 注释）纪律不齐 | 函数尾 `debug!(pool, boosted, taken)` | safe |
| ods.rs:139-141 | anchors 空早退分支无测试钉「anchors 空 → 不查库直接空集」 | 补一行纯判据测试（空 anchors 不触达 PG 可mock断言或至少钉早退存在） | safe |
| ods.rs:160-165 | 端点归一靠 `warehouse_asset` 支持限定名（registry/mod.rs:170 `warehouse_table_parts` 剥库名），L41 注释说「两种形态都能中」但没点名靠的是谁 | 注释点名 `warehouse_asset` 的归一职责 | safe |
| ods.rs:96-101 | forms 只喂「裸名+目录限定名」两种形态；边表若存反引号/大小写变体（registry/mod.rs:156-160 `catalog_ident` 会 trim 的形态）则不中——两处归一标准不一 | forms 构建前复用 `catalog_ident` 归一 | test |

## crates/semantic/src/ingest/schema_sync.rs（14 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/semantic/src/ingest/schema_sync.rs:29 | 注释「kept（否则清理会把刚跳过的表删掉）」理由方向反了：代码效果恰是「排除出 kept ⇒ 被清理删掉」，而 prune_stale_docs 的立意（L98-99「规则收紧后不留幽灵」）正是要删——注释把因果说反，误导后来者把备份表加回 kept | 改写为「kept 里不留它们，好让清理把收紧前已入库的备份表文档删掉」 | safe |
| crates/semantic/src/ingest/schema_sync.rs:48-50 | 快照为空（采集失败/权限收紧）时 kept=[]，`!= ALL('{}')` 恒真 → 该 ds 的 table_doc/column_doc 被全删且零日志——一次采集异常即清空注册表 | `kept.is_empty()` 且 `snap.tables.is_empty()` 时跳过 prune 并 warn | test |
| crates/semantic/src/ingest/schema_sync.rs:50-63 | prune（删）与逐表 upsert（写）不在事务里：中途 `?` 失败 = 旧的删了新的没写完，注册表残缺到下次 sync | 整轮包事务，或先写后删 | test |
| crates/semantic/src/ingest/schema_sync.rs:56 | 每张表都 `snap.columns.iter().filter(...)` 全扫一遍，O（表数×列数） | 进循环前按表名分组一次（HashMap<&str, Vec<&ColumnInfo>>） | safe |
| crates/semantic/src/ingest/schema_sync.rs:57-62 | 每表 1 次 + 每列 1 次单句 upsert，一轮 sync 上千次 round trip | 批量（多行 VALUES 或 unnest 数组） | test |
| crates/semantic/src/ingest/schema_sync.rs:57,60 | 单表/单列 upsert 失败 `?` 直接中止整轮 sync，剩余表全丢且无失败清单 | warn 记录继续，收尾返回失败数 | test |
| crates/semantic/src/ingest/schema_sync.rs:73,145 | 同一条列注释 `sanitize_comment` 算两遍（search_doc 一次、upsert_column_doc 一次） | 循环里洗一次，两处复用 | safe |
| crates/semantic/src/ingest/schema_sync.rs:73 | 列名 `c.name` 不经过 sanitize 直接拼进 search_doc——F4 注释（probe.rs:153-159）自认上传源列名来自用户 Excel 表头；注释洗了、列名没洗，prompt 注入面留一道缝 | 确认上传侧列名已规范化（c0/c1…），否则同洗列名 | test |
| crates/semantic/src/ingest/schema_sync.rs:73-75 | 空注释列产生尾随空格（L182 测试把 `"c1 "` 钉成预期），search_doc 里全是双空格/尾空格 | filter 掉空段再 join，同步改测试 | test |
| crates/semantic/src/ingest/schema_sync.rs:20 | 注释「`comment` 与 `search_doc` 都已过 `sanitize_comment`」与列名未洗相矛盾，overclaim | 注释收窄为「注释部分已过清洗」 | safe |
| crates/semantic/src/ingest/schema_sync.rs:100-106 | 两条 DELETE 顺序执行无事务：table_doc 删完 column_doc 失败 = 列孤儿 | 包事务 | test |
| crates/semantic/src/ingest/schema_sync.rs:121-129 | drop_schema_docs 同样无事务，且删了多少行零日志（排查孤儿问题时无据可查） | 事务 + info! rows_affected | test |
| crates/semantic/src/ingest/schema_sync.rs:39-65 | 成功路径零日志，(n_tables, n_cols) 只靠调用方记得打印 | 函数内收尾 info! 一句 | safe |
| crates/semantic/src/ingest/schema_sync.rs:44 | 返回值 `(usize, usize)` 裸元组，调用方靠位置解 | 命名小结构或文档注明顺序 | safe |

## acl.rs（14 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| acl.rs:6-9 | 模块头注释说「`space_id == viewer.login` 视为个人空间可写」，与 L172「不按 `space_id == login` 放行」直接矛盾（头注释是过期文案） | 改头注释为现行口径：owner 恒可写，其余必须显式 `perm='write'` | safe |
| acl.rs:4 | 头注释称 `visible_docs_sql()` 是「给检索内联进 SQL 的子查询片段」，实际被内联的是宏 `visible_docs!()`（L244-246 自己写得对），`visible_docs_sql()` 只服务测试 | 头注释改成「内联用宏，`visible_docs_sql()` 是同文本的运行时视图」 | safe |
| acl.rs:179-191 | `SELECT count(*)` + `fetch_optional` + `map_or(0)`：count 恒返回一行，optional/map_or 是误导性写法，还多扫一次聚合 | 改 `SELECT EXISTS(SELECT 1 FROM kb.space s WHERE …)` 直接 `fetch_one::<(bool,)>`，判据条件一字不动 | test |
| acl.rs:201-214 | 同上，`space_readable` 同款 count/optional 模式 | 同上改 `EXISTS`，判据不动 | test |
| acl.rs:63-69 | `Grantee::parse` 接受空串/纯空白 id，会落出 `grantee=''` 的废授权行 | trim 后为空则返回 None | test |
| acl.rs:32-39 | `AclScope::parse` 对大小写/空白敏感，注释未说明调用方须先归一 | 注释写明「输入须已 trim+小写」，或在 parse 内做归一 | safe（注释）/test（归一） |
| acl.rs:122-136 | `grant` 的 `ON CONFLICT DO NOTHING` 让「授权已存在」与「新建成功」不可区分，审计日志没法如实记录 | 返回 `rows_affected` 或 `RETURNING` 判空，把两种结果交给调用方 | test |
| acl.rs:122 | `grant` 不校验 target 存在性，space_id 打错字就落一条永远匹配不上的孤儿授权 | 文档注明「目标存在性由调用方保证」，或加 `SELECT EXISTS` 前置校验 | safe（注释）/test（校验） |
| acl.rs:138-152 | `revoke` 丢弃 `rows_affected`：撤一条不存在的授权也返回 Ok，静默 | 把影响行数返回给调用方用于提示「授权不存在」 | test |
| acl.rs:154-168 | `list_target` 无 LIMIT，单目标授权行数失控时全量拉回 | 加一个宽裕上限（如 1000）或注释说明行数有外部约束 | safe |
| acl.rs:111-120 | 手写 `FromRow` 三次 `try_get`，字段名与列名一致，可用 derive | 若 sqlx macros feature 已开，换 `#[derive(sqlx::FromRow)]` | safe |
| acl.rs:181 vs 203 | 两段 SQL 的 grantee/role 谓词逐字重复，日后改一处忘一处就是越权洞 | 抽公共 `concat!` 片段拼进两条 SQL（运行时串不变；L307-316 锚点测试继续钉住） | test |
| acl.rs:247-258 | 宏片段不含 enabled/status/生效期过滤，这份职责隐式推给每个内联者，宏文档没写 | 宏文档加 ⚠️：文档生命周期过滤是内联者的义务，并举 kg.rs 为例 | safe |
| acl.rs:217 | `doc_for_viewer` 的「不区分不存在」只靠注释，测试里没有针对「不存在与无权返回同一错误」的钉子 | 补一条单测断言两种情形同一 `Forbidden` 文案 | test |

## ast.rs（14 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| ast.rs:9 | `use core::ops::ControlFlow` 而 guard.rs:6 用 `std::ops::ControlFlow`，仓内不统一 | 统一为 `std::ops::ControlFlow` | safe |
| ast.rs:42 | 每个嵌套 Query 都 `cte_scopes.last().cloned()` 克隆整个外层 CTE 集 | 用 `Rc<HashSet>`/作用域栈共享，或注释说明规模上限 | safe |
| ast.rs:83-84 | 末段被 lowercase 两次（`parts` 收集一次、`table` 又算一次） | `let table = parts.last().cloned().unwrap_or_default()` | safe |
| ast.rs:90-95 | `parts` 为空时（理论不可达）93 行 `parts[0]` 会 panic；93 行之前只有 `parts.len()==1` 的短路边界 | 开头加 `if parts.is_empty() { return Continue }` 防御 | safe |
| ast.rs:93,21 | 限定名 `db.t_x` 的 `real_tables` 收的是 `parts[0]`（**库名**），与 21 行注释「真实表名」不符；下游已实测踩坑并绕开（direct.rs:3088-3089 注释） | 只改注释：写明「限定名按首段（库名）入账」；改代码会动权限登记语义，不动 | safe |
| ast.rs:159 | 注释「派生表别名不会进 aliases（TableFactor::Derived 无 name）」措辞不准：Derived 有 alias，只是没有**表名** | 注释改为「Derived 分支不匹配，故不进」 | safe |
| ast.rs:166 | 排序理由写「`aliases` 是 HashMap，迭代顺序不定」——但排的是 `real_tables`（Vec，顺序本确定）；真正理由是输出规范化 | 注释改成「排序+去重使输出与遍历顺序无关」 | safe |
| ast.rs:143-195 | 三个视图函数各自 `walk`：corrector.rs:80-86（collect+table_names_of）与 postgres.rs:146-156（function_names_of+table_refs_of）都对同一 SQL parse 两次 | 增加一个返回全部视图的组合 API（纯增量），下游逐步迁移 | test |
| ast.rs:201,205 | `trim_matches('`')` 是死代码：sqlparser 的 `Ident.value` 本就去引号（同见 caliber.rs:138,804,1077） | 删除或改注释为「防御性」；全仓统一一处说明 | safe |
| ast.rs:198-239 | `collect_where_cols` 无 `Expr::Case` 分支：WHERE 里 CASE WHEN 内的列静默漏采 | 补 Case 分支递归（miss→收） | test |
| ast.rs:215 | `InSubquery` 只收左端列，子查询内部的表/列不进结果（注释未声明该边界） | 注释声明，或递归子查询 selection | test |
| ast.rs:227-236 | Function 只收 `List` 且只收 `Unnamed` 参数，`Named` 参数列漏采 | 补 Named 臂（行为扩展） | test |
| ast.rs:198-239 | 递归无深度上限，极端嵌套输入理论可爆栈（同 caliber 的 Grab/scan_query） | 注释声明「输入已过 gate 的 AST，深度受 parser 限制」，或加深度计数 | safe |
| ast.rs:116-141 | `collect_table_commands` 的 `_ => {}` 吞掉未来新 SetExpr 变体，编译期无感 | 加注释说明「新变体默认识漏方向」 | safe |

## crates/kernel/src/answer.rs（14 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/kernel/src/answer.rs:17 | 字段枚举注释「顶层键 = route + 变体展开的键 + 可选 view/subs + elapsed_ms」漏了 L34 的 `trace_id`——注释与代码不符 | 补上 trace_id | safe |
| crates/kernel/src/answer.rs:19,40,62,70 | Answer/AnswerBody/SubAnswer/Citation 全无 `Debug`，排障只能 serde 后打印 | `#[derive(Debug, Serialize)]`（零 wire 影响） | safe |
| crates/kernel/src/answer.rs:30 | `elapsed_ms: u128`：serde_json 对 >u64::MAX 的 u128 默认拒绝序列化（运行时错）；现实值到不了，但 u64 与其它 crate 毫秒字段更一致，合法取值 wire 字节不变 | 改 u64 并全仓调用点收敛 | test |
| crates/kernel/src/answer.rs:41 | 「与**今天的** AskResult.sql 同义」是时间敏感考古措辞，随时间腐化 | 改「与迁移前 server 侧 AskResult.sql 同义」 | safe |
| crates/kernel/src/answer.rs:56 | Composite 的 `summary: Option<String>` 无 skip_serializing_if → None 时上线 `"summary": null`，与全文件其它 Option「None 不上线」纪律不一致（wire 形状不能动） | 注释钉「null 上线是历史兼容，勿『顺手』加 skip」 | safe |
| crates/kernel/src/answer.rs:68 | `_DECISIONS.md` 引用不写全路径，仓内唯一一份在 `docs/superpowers/plans/`，搬目录即死链 | 写全路径 | safe |
| crates/kernel/src/answer.rs:8-10 | 「T9 迁入」「K2 首个消费者」任务编号无索引可查，新人无法反查 | 改成功能性描述或指到 docs 文档 | safe |
| crates/kernel/src/answer.rs:113-126 | 只有 `Answer::text` 一个构造器，Table/Composite 全靠各生产者手写字面量（agent/ask.rs 多处），字段演进纯靠编译器兜底 | 补 `Answer::table(...)`/`Answer::composite(...)` 构造器 | safe |
| crates/kernel/src/answer.rs:24 | 「Table 路径恒 Some」无任何代码保障（`view` 是 pub 字段，谁都能给 None） | 构造器强制，或注释改成约定声明 | safe |
| crates/kernel/src/answer.rs:49 | 「角标 = citations 下标 + 1，不存字段」前端契约只存在这条注释里，web 侧无交叉引用 | web 渲染处加同一句交叉注释 | safe |
| crates/kernel/src/answer.rs:46-47 | `row_count` 语义未钉：是 wire 内行数还是总行数？各生产者现在都是 `rows.len()`，但注释不写就会漂 | 文档钉「row_count == rows.len()（截断时 truncated=true）」 | safe |
| crates/kernel/src/answer.rs:73-76,110 | `chunk_id: i64` / `page: Option<i32>` / `span: Option<u32>` 三个数值三种类型，无来源说明 | 注释标注各自来源（PG bigint/int4、计数非负） | safe |
| crates/kernel/src/answer.rs:70-111 | Citation 15 个字段逐个手写 skip 属性，新增字段忘加 skip 就悄悄改 wire；现有 golden 只查部分键 | 加「全默认字段时 citations[0] 键集合」golden | test |
| crates/kernel/src/answer.rs:155 | 测试 fixture `doc_updated_at` 是 `"2026-08-06 00:00:00+00"` 裸字符串，格式契约（PG timestamp 输出形态）无校验无注释 | 注释说明格式来源，或 golden 断言该串形态 | safe |

## crates/connector/src/fixed.rs（14 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/connector/src/fixed.rs:78-87 | `classify` 保留原始 sqlx 文案，mysql.rs `sqlx_err`(1055-1073) 却把 access-denied 归一为「数据库权限不足」、decode 归一为「数据库返回数据无法解码」—— 同 crate 两条通道错误口径分裂，1053 行注释还自称「同口径」 | 两函数合并或注释说清「变体同口径、文案不同」 | test |
| crates/connector/src/fixed.rs:228-267 | `PgStmt` 三个方法全无超时也无慢日志，与 `FixedStmt`（8s 超时 + 500ms warn）不对称 —— 本地 PG 挂死时 handler await 无上限 | 加 PG 侧 timeout 常量 + 同款 slow-log | test |
| crates/connector/src/fixed.rs:122-131 | `expand` 不校验模板含 `{in}`：无标记时静默原样返回（285 行测试钉死了该行为），调用方照绑 n 个值 → 到数据库才报 bind 数不匹配，错误归类为 Query 而非「调用方错」 | expand 时无 MARK 记 config 错；同步改 285 行测试 | test |
| crates/connector/src/fixed.rs:122-131 | `expand(n)` 无上界：n 极大时拼出超长 SQL 顶爆 `max_allowed_packet`，错误在远端才暴露 | 加上限（如 10_000）记 config 错 | test |
| crates/connector/src/fixed.rs:161-163 | `started.elapsed()` 调两次（判断 + 日志字段），两次值可不同；183-185 同病 | `let elapsed = started.elapsed();` 复用 | safe |
| crates/connector/src/fixed.rs:162 | 慢查询 warn 不带 SQL 指纹 —— 全是静态 SQL，不带文本分不清是哪条慢（bind 值在 args 里、SQL 本身无数据，可安全记） | 日志加 `sql = &sql[..sql.len().min(80)]` | safe |
| crates/connector/src/fixed.rs:145-187 | `fetch_all`/`fetch_optional` 仅末尾一行不同，整段超时+慢日志重复 | 抽共用 `run(self, fetch: impl FnOnce)` | safe |
| crates/connector/src/fixed.rs:52 | `out.push_str(&format!("${next}"))` 每个占位符一次临时 String 分配 | `use std::fmt::Write; write!(out, "${next}")` | safe |
| crates/connector/src/fixed.rs:138-141 | `err` 已置后 `bind` 仍继续编码后续参数（结果必然丢弃） | `if self.err.is_some() { return self; }` 早退 | safe |
| crates/connector/src/fixed.rs:42 | 模板无 `{in}` 时也算 `max_dollar`（结果用不上）；与无标记静默问题同源 | 先 `find(MARK)` 短路 | safe |
| crates/connector/src/fixed.rs:65-71 | `max_dollar` 会把模板字符串字面量里的 `$k`（如 `'US$5'`）误计为参数上限 —— 39 行 doc 只约束了固定参数位置，没提字面量坑 | doc 补一句约束（静态模板由评审保证） | safe |
| crates/connector/src/fixed.rs:13 | 「两份 50 行的具体实现」与现状不符（两实现各 ~85/78 行、文件 360 行）—— 注释随演进漂移 | 改为不写死行数 | safe |
| crates/connector/src/fixed.rs:313 | 四个测试重复 `connect_lazy("mysql://u:p@127.0.0.1:1/db")`（322/336/349 同） | 抽 `lazy_mysql()`/`lazy_pg()` 小助手 | safe |
| crates/connector/src/fixed.rs:39 | 「固定 `$k` 都写在 `{in}` 之前」只是 doc 约定，违反时静默撞号（编号与 bind 错位）；可在 expand 时 debug 断言 | `debug_assert!(tpl.find(MARK) > 最后一个 $k 位置 或无 $k)` | safe |

## source.rs（14 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| source.rs:71 | 注释承诺「`Some(0)` 是合法的最紧档（恒空结果）」，但实现侧 `to_table` 先 push 后判（postgres.rs:295-309、mysql.rs:1142）使 max=0 实返 1 行——契约与实现矛盾 | 修实现为 take(max)（推荐），或改契约措辞 | test |
| source.rs:79 | `clamp` 无 `#[must_use]`；返回值被丢弃=策略静默失效，正是本类型要防的事故 | 加 `#[must_use]` | safe |
| source.rs:106 | 「超出即截断，不报错」未说明是实现侧拉全量后内存截断（postgres.rs:237-241）、无 DB 端 LIMIT 注入；读者易误以为服务端截断 | 文档补一句截断位置与内存含义 | safe |
| source.rs:29 | `RowSet` derive `Debug`：任何 `{:?}` 日志都会打出全部行数据（业务值） | 手写 `Debug` 只打 columns/行数/redacted | test |
| source.rs:88 | 「具名 MySQL 是 ds_id 断链的头号成因」是残句，因果不明 | 重写为完整句子 | safe |
| source.rs:95-97 | `is_warehouse` 默认 false 实际承载了 PostgresSource 的行为（它没 override）——trait 默认实现替单侧实现表态 | postgres.rs 显式 override 或此处注释点名 PG 走默认 | safe |
| source.rs:104 | 「空操作实现=静默丢地上」只有 Fake 测试（205-208）守，新实现无任何强制 | trait 文档加实现检查清单一行，或给两实现补 set→生效断言 | safe |
| source.rs:196 | 契约「超出即截断不报错」无测试：Fake 不截断也无相关断言 | Fake 实现截断并断言 max=1 只返 1 行 | test |
| source.rs:151 | 测试 Fake 的锁容错（`into_inner()`）与生产实现的 expect-panic（postgres.rs:223/235、mysql.rs:133）示范两种相反策略 | 统一口径（建议生产侧容错化） | test |
| source.rs:181 | `GuardConfig::new(200, &[])` 的 200 是 magic number | 注释「与 `dms_agent::MAX_ROWS` 同档」 | safe |
| source.rs:22-26 | `SourceKind` 无 `Display`；`DsSpec` derive Debug（registry.rs:23）进日志时只能以 Debug 形态出现 | 加 `Display`（小写源名）供日志使用 | safe |
| source.rs:62 | `SchemaSnapshot.columns` 每列重复克隆一份表名 `String`，千列大库=千份重复分配；58 行注释论证了不分组、未论证字符串重复 | 注释补「已知浪费、暂不动」，或改 `Rc<str>` | safe |
| source.rs:114-122 | `explain` 的 `Ok(None)` 抖动语义在文件头第 2 条与方法文档重复表述两遍，未来改一处漏一处 | 方法文档改为引用文件头条目 | safe |
| source.rs:65-68,214 | 文档跨 crate 引用 `dms_semantic::...DsPolicyConfig` 与 `dms_agent::MAX_ROWS/EXEC_TIMEOUT`，对方改名无编译器守护 | 弱化为模块级描述（「配置面在 semantic 注册表」） | safe |

## insight_api.rs（13 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| insight_api.rs:107 与 249 | `analysis` 与 `report` 各自逐字段拼同一个 `dms_agent::Reading`，两处漂移风险 | 抽 `fn reading_of(question, sql, columns, rows, row_count, caliber_note) -> Reading<'_>` 共用 | safe |
| insight_api.rs:107 | `row_count.unwrap_or(req.rows.len())` 全信调用方：`row_count < rows.len()` 时 `report_md` 走 else 分支打印「共 X 行」却列出更多行，文案自相矛盾 | `let total = req.row_count.unwrap_or(req.rows.len()).max(req.rows.len());` | test |
| insight_api.rs:183 与 199 | 行数说明写「下表为前 {rows.len()} 行」，但表体只 `take(50)`：rows 61–200 行时文案与实物不符（且 row_count==rows.len()>50 时静默只给 50 行、连截断说明都没有） | 文案统一按 `min(rows.len(), 50)` 生成 | test |
| insight_api.rs:199 | 魔法数 50（表体截断） | 提常量 `const REPORT_TABLE_ROWS: usize = 50;` | safe |
| insight_api.rs:199-210 | 行单元格数与 `columns` 不一致时输出锯齿 markdown 行（列少了表就歪） | 渲染前校验行长，不符的行跳过或补齐空单元格 | test |
| insight_api.rs:218-220 | `req.sql` 含 ```` ``` ```` 时会顶破围栏，后续 HTML 渲染走样 | 围栏升四级 ```` ```` ```` 或对 sql 中的反引号序列转义 | test |
| insight_api.rs:233-236 | `conv_id.parse::<i64>()` 接受 `"-5"`（合法数字但非主键），随后落 403 而非 400，错误文案「必须是会话主键数字」此时名不副实 | parse 后加 `cid <= 0` 判 400 | test |
| insight_api.rs:253 | `title` 取 question 前 40 字符但不 trim，前导空白/换行直接进 HTML `<title>` | `.trim()` 后再 take | safe |
| insight_api.rs:261 | `save_artifact` 传的是原始字符串 `req.conv_id`，而非刚解析校验过的 `cid`（`"012"` 这类写法原样落库） | 传 `cid.to_string()` 或让 save_artifact 收 i64 | safe |
| insight_api.rs:239 与 263 | 读会话归属（只读）与写产物（写）共用同一条 warn 文案「insight 服务端读写失败」，排障时分不清是哪一步 | 拆成「读会话归属失败」/「保存报表失败」两条 | safe |
| insight_api.rs:94-101 与 230-242 | 两个 handler 的「resolve_identity → 401 → 业务校验」前段高度重复 | 抽公共身份核验小函数（401 文案保持逐字） | safe |
| insight_api.rs:291 | `split("#[cfg(test)]").next().unwrap_or("")`：若切分失败得空串，下面两条 `assert!(!code.contains(bad))` 恒绿——哑断言形态（本仓自己反复批过的那类） | `unwrap_or("")` 改 `expect("测试模块必然存在")` 并补 `assert!(!code.is_empty())` | safe |
| insight_api.rs:225-229 | `report` 不校验 `question`/`sql` 非空：空问题+空 SQL 也能固化出一张空报表 artifact | 入口处 trim 后判空返 400 | test |

## ds_api.rs（13 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| ds_api.rs:108 | 注释说「这里的 `retain`」但代码是 `rows.iter().filter().collect()`，注释与代码不符 | 注释改为「filter 交集」 | safe |
| ds_api.rs:116 | `&[p.role_code.clone()]` 为构造单元素切片克隆整个 String | `std::slice::from_ref(&p.role_code)` | safe |
| ds_api.rs:122-123 | `visible.contains(&r.ds_id)`：`Vec<String>` 线性查找套在 filter 里，O(n×m) | `visible` 转 `HashSet<&str>` 再 filter | safe |
| ds_api.rs:229 | `enrich_dms_snapshot(...).await.unwrap_or(0)` 静默吞错：注释富化失败=注释列悄悄缺失，无任何日志 | Err 分支 `warn!`（返回 0 不变） | safe |
| ds_api.rs:272-276 | `check_dsn_ref` 错误文案回显被拒的 `{s}`：误填明文 DSN（含口令）时口令随 400 响应回显到浏览器/前端日志 | 文案去掉原值，只描述规则 | test |
| ds_api.rs:128-139 | `DsUpsertReq` 的 `name`/`description` 无长度上限，且 description 会经 list 全量回显 | 加上限校验（如 name≤128、description≤2000） | test |
| ds_api.rs:288-293 | `kind` 与 `dialect` 各自合法性校验但不校验组合：`kind=mysql + dialect=postgres` 可通过，产出自相矛盾的注册行 | 显式 dialect 时要求 `source_kind` 结果一致 | test |
| ds_api.rs:190 | probe 的 `max_conn: 2` 魔数与注释「独立短连接」不符（为何是 2？） | 注释说明或改 1 | safe |
| ds_api.rs:216-220 | sync 的 422 文案含内部代号「K3-B」，终端用户看不懂 | 文案面向用户，代号移到注释 | safe |
| ds_api.rs:255,285 | 404/400 文案回显用户输入的 `id`/`ds_id`（无注入风险，但与「内部错误不回显原文」口径不一） | 泛化文案或注释说明刻意回显 | safe |
| ds_api.rs:119-123 | 先全量 `list_datasources` 再内存交集：源多时把不可见行也拉回应用层 | 可见性 SQL 直接 JOIN/ANY 过滤（保持单一 ACL 实现不变） | test |
| ds_api.rs:171 | `st.sources.close(...)` 结果完全忽略且上下文无注释，若 close 内部有失败路径无从感知 | 注释说明 close 语义/或 warn | safe |
| ds_api.rs:417-428 | 源码闸测试只扫 `err(StatusCode::X, e)` 三个字面形态，漏 `err(code, e)` 变量形态，防回归网有洞 | 测试补变量形态模式 | safe |

## crates/agent/src/answerers/hits.rs（13 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/agent/src/answerers/hits.rs:86-89 | 注释说「`AskCtx` 今天没有 `t0` 字段，故成员内自取」，但 89 行就是 `let t0 = cx.t0;`，ctx.rs:56 确有 `pub t0`——注释与代码直接矛盾 | 删/改这三行注释为「t0 来自 AskCtx，覆盖整次单问」 | safe |
| crates/agent/src/answerers/hits.rs:116 | 红线闸门拒 → 静默 `None`（注释 106-107 说是刻意），但同文件 125-132 刚为「静默吞错」付过账；零成本留痕不违约 | 加 `tracing::debug!`（非 warn，保持「静默回落」语义但可排查） | safe |
| crates/agent/src/answerers/hits.rs:133-138 vs 217-236/302-321 | 主查询失败 warn 带 `route`，`fetch_detail`/`fetch_sales_context` 的 warn 不带 route/target，同类日志形状不齐 | 两处 warn 补 `route = %route`（参数透传） | safe |
| crates/agent/src/answerers/hits.rs:142-147 | 取数完成 info 记了 rows 但没记 `rs.rows.len() >= MAX_ROWS` 的截断态，排查「数据是不是被截了」要多查一步 | info 加 `truncated = rs.rows.len() >= MAX_ROWS` 字段 | safe |
| crates/agent/src/answerers/hits.rs:154 | `resolve_document(cx.question, false)` 重扫问句——上游 direct-doc 产出方已识别过一次，纯函数重扫浪费 CPU | 注释说明「重扫是有意的隔离」或在 DirectHit 上带 family 透传（后者改 wire 形状，故只建议注释） | safe |
| crates/agent/src/answerers/hits.rs:243-258 | `header_pairs` 无条件 clone 一份 Entity pairs，但只有 `replace_primary` 分支（288 行）用；聚合路径（264-273 提前 return）白克隆 | 把 header_pairs 计算挪进 `replace_primary` 分支内 | safe |
| crates/agent/src/answerers/hits.rs:280-285 | `find` 只保留**第一个** Entity | Kpis 块；若头视图同时有 Entity+Kpis，另一个被静默丢弃且 287 行因 blocks 非空不再补 Entity——头卡信息丢失的边界 | 改 `filter` 保留全部前置块，或对「多块」情况加 debug_assert |
| crates/agent/src/answerers/hits.rs:335-344 | `fetch_prev` 闸门失败静默 None；同族 `fetch_detail`/`fetch_sales_context` 闸门失败都有 warn——三类基期/补充取数留痕口径不一 | gate 失败分支补 `tracing::debug!`/`warn!` 一条 | safe |
| crates/agent/src/answerers/hits.rs:342 | `.ok()?` 把基期取数错误整个丢掉——正是本文件 125-132 注释痛陈过的「静默吞错」模式，只是发生在 prev 半 | `.map_err( | e |
| crates/agent/src/answerers/hits.rs:350-372 | `patch_kpi_delta` 在去重判断（351 行）**之前**执行：prev 与 comparisons 撞同名标签时视图被打两次补丁、comparisons 只入一条——两处口径不一致 | 把 dedup 判断提到 patch 之前（确认 patch 幂等性后） | test |
| crates/agent/src/answerers/hits.rs:350,360 | 同一 `label` 连续 `to_string()` 两次（patch 一次、push 一次） | 先 `let label = label.to_string();` 复用 | safe |
| crates/agent/src/answerers/hits.rs:354 vs 365-371 | 零判用两套 epsilon：pct 用 `f64::EPSILON`（≈2.2e-16，对金额形同虚设，prev=1e-9 也会算出天文数字环比），dir 用 1e-6——同一函数内阈值不一致 | 统一成一个业务 epsilon（如 1e-6），并补 prev≈0 的用例 | test |
| crates/agent/src/answerers/hits.rs:378-384 | `cell_num` 对 `"1,234,567.89"` 千分位字符串 parse 失败 → None → 环比静默消失（392 行注释最怕的静默消失） | parse 失败时去掉 `,` 再试一次，配测试 | test |

## ops_caliber.rs（13 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| ops_caliber.rs:137-141 | `ops_activity_sessions` 的 SUM 未包 `COALESCE(…,0)`：空集返回 NULL，而 sales(142)/cost(143) 返回 0——同族指标空集语义不一致 | 包 `COALESCE(SUM(...),0)` 或注释说明 NULL 是有意 | test |
| ops_caliber.rs:279-302 | `direct_metric` 每次调用都 `metrics()` 重建 12 条指标（每条含多次 format!/String 分配，province CASE 重复生成十余次），问句热路径上的纯浪费 | `OnceLock<Vec<Metric>>` 缓存一次构建 | safe |
| ops_caliber.rs:288 | consumed 里的「呢」「总共」「一共」与 `lexicon.rs:36,64` 的 `STRIP_WORDS` 重复（has_residue_with 反正会再剥一遍），只有「吗」「了」是增量 | 删重复词或注释说明为何重复 | safe |
| ops_caliber.rs:331-359 | `seed_metrics` 两个循环各自调用 `metrics()`，agg 字符串构建两遍；第二个 UPDATE 循环也不查 `rows_affected`（与 insert 循环漂时静默） | 合并为单循环一次遍历，UPDATE 0 行时 warn | safe |
| ops_caliber.rs:60-61,68 | `'2026-06-01'` 起算日在 `activity_valid`/`inspection_valid` 两处硬编码，TERMS(306) 文案里还有第三处，版本升级要改三个点 | 提 `const OPS_EPOCH: &str = "2026-06-01"` 供 SQL 拼接 | safe |
| ops_caliber.rs:77-82 | 问句无时间词时 `time_and` 只留注释不加过滤：`direct_metric` 返回的 SQL 会按**全时段**执行并直接答出，无任何提示；466-478 的测试只覆盖有时间分支 | 无时间时拒直答（None）或测试钉住全时段语义 | test |
| ops_caliber.rs:137-272 | `metrics()` 里 12 处 `metric_expr(...).unwrap()`：新增 Metric 忘了加 `metric_expr` 分支时运行期 panic（seed 与每次 direct_metric 都炸） | `unwrap` 改 `expect("metric_expr 缺分支： code")`，或让 metric_expr 对 metrics() 全 code 穷尽并由测试断言 | safe |
| ops_caliber.rs:429-438 | 三条 `custom_comment` UPDATE 不查 `rows_affected`，表名打错静默（seed.rs:181-192 已有对照模式） | 收集 0 行表名并 `tracing::warn!` | safe |
| ops_caliber.rs:45-56 | `activity_region` 生成的 CASE 串里 REPLACE 链写两遍（IN 列表与 THEN 各一份），且 `activity_agg`/`promoter_agg` 里 valid 与 region_and 又各算一次该字符串 | SQL 层可用子查询/复用变量；Rust 层把 `activity_region(alias)` 结果存局部变量复用 | safe |
| ops_caliber.rs:348-352 | 维度白名单靠 `code.starts_with("ops_activity")` 字符串前缀分流，今后新增前缀不符的活动类指标会静默落到巡店维度组 | 改显式 （代码→dims） 表或加测试钉住映射 | test |
| ops_caliber.rs:286 | `max_by_key((w.chars().count(), m.name))` 的词长平手时按名字字典序取最大，确定性但语义任意；两个指标同词长命中时选谁没有注释 | 补一行注释说明平手判据是有意的 | safe |
| ops_caliber.rs:106-124 | `activity_agg`/`promoter_agg` 对 `activity_valid`/`time_and`/`activity_region` 的结果全部即时 format!，三个 agg 函数各自重复拼接相同片段，可读性差 | 抽一个 `struct Ctx{valid,time,region}` 一次算好传入 | safe |
| ops_caliber.rs:326-329 | `EDGES` 只有 2 条且无任何守卫测试（对照 seed.rs:740 有血缘钉住测试）；巡店/活动域的 customer 边被删时无断言会红 | 加源码包含断言钉住这两条边 | test |

## model.rs（13 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| model.rs:1-357 | 全文件 CRLF/LF 行尾混杂（1-16 行 `\r`、17-33 行无 `\r` 等），diff 噪音源 | 统一行尾（caliber.rs 同病） | safe |
| model.rs:62-124 vs 126-191 | `load_metrics` 与 `load_metric_policies` 同表同谓词各查一遍 `meta.metric`，一轮问答两份往返 | 一次加载、两个投影 | test |
| model.rs:68-80 | 11 元组手写类型标注挤在 `let rows:` 上 | 本地 struct 或换行格式化 | safe |
| model.rs:132-146 | 13 元组同款 | 同上 | safe |
| model.rs:185 | `catalog_allows_metric_dimension` 内部每个 dimension 重跑一次 `source_refs(source_table)` 解析（mod.rs:469） | 循环外解析一次传入 | safe |
| model.rs:226-229 | `load_join_edges` 无 `ORDER BY`（对比 84 行有），输出序随库漂 | 补 `ORDER BY left_table, right_table` | test |
| model.rs:314-315 | `load_table_scope_rows` 无 `ORDER BY` + caliber.rs:531「note 首次登记者胜出」⇒ 同表多条 scope 时 human 文案随返回序漂 | 补 `ORDER BY`（如 `table_name`） | test |
| model.rs:339-341 | `load_table_snapshots` 无 `ORDER BY` ⇒ caliber.rs:478 按序 push 的 `RequireLatest` 规则序随库漂 | 补 `ORDER BY table_name` | test |
| model.rs:301 | `match_kind.unwrap_or_default()`：NULL→`""`，而 275-277 注释说装配只认 `eq`/`like`，`""` 落进哪一侧无声明 | 核实下游对 `""` 的处理，改 `unwrap_or("eq")` 或 DB 默认值 | test |
| model.rs:267-278 | `ValueRef` 与 lexicon.rs:21-27 `ValueMap` 字段完全相同的两份行类型 | 合并为一个类型 | safe |
| model.rs:280-304 vs lexicon.rs:119-145 | `load_value_map`/`load_value_maps` 同表两份加载：过滤口径（`catalog_allows_table` vs `catalog_allows_column`）、`ORDER BY` 有无都不同 | 至少注释互指；统一为一份更佳 | test |
| model.rs:18,37,46,54,244,251,267 | `MetricDef`/`MetricPolicy`/`DimensionDef`/`JoinEdge`/`TableScope`/`TableSnapshot`/`ValueRef` 全无 `Debug`，排障只能逐字段打印 | 补 `#[derive(Debug)]` | safe |
| model.rs:63-67,127-131,194-198,221-225,281-285,309-313,334-338 | 七处 `format!("{}{}", ds_pred(1), xxx_pred_at("",1))` 重复拼谓词 | 抽 `scoped_pred(live_pred)` helper | safe |

## crates/semantic/src/ingest/autodiscover/probe.rs（13 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/semantic/src/ingest/autodiscover/probe.rs:41 vs 108 | 「人工已覆盖」大小写不一贯：vm 键入库时 lowercase（L108）、covers 也 lowercase 比较（L39），而 dims 的 (source_table, expr) 不 lowercase、contains 大小写敏感——大写表名的维度漏判覆盖 | dims 入库即 lowercase | test |
| crates/semantic/src/ingest/autodiscover/probe.rs:41 | `src.contains(table)` 子串判：`t_goods_category` 的维度会盖住候选表 `t_goods`（前者 contains 后者）——人工优先变成人工误伤，合法候选被静默跳过且只进 skipped_manual 计数 | 等值匹配或词边界匹配 | test |
| crates/semantic/src/ingest/autodiscover/probe.rs:41 | `expr.contains(col)` 子串判：候选列 `b_type` 被 expr 里的 `x_b_type` 命中，同上静默误跳过 | 至少按反引号/词边界匹配列名 | test |
| crates/semantic/src/ingest/autodiscover/probe.rs:39 | covers() 每次调用分配两个 String（to_lowercase），循环内每候选一次 | 在调用外对候选 (table,col) 预 lowercase 一次 | safe |
| crates/semantic/src/ingest/autodiscover/probe.rs:71-82 | candidate_columns 不带 enabled/table_asset_live 谓词：A20 被人工勾掉的表照样被探、被注册进 value_map/dimension；而名称型通道 load_value_domains（lexicon.rs:39-44）有 live 谓词——两条候选通道口径不一 | 拼同一 `table_asset_live_pred_at` 谓词 | test |
| crates/semantic/src/ingest/autodiscover/probe.rs:76 | 后缀正则 `~` 大小写敏感：`…_CODE`/`…_Type` 这类大写后缀列永远不进候选，静默漏发现 | 改 `~*` 或注释写明「列名按小写假设」 | test |
| crates/semantic/src/ingest/autodiscover/probe.rs:102-113 | manual_covered 两条 PG 查询串行 await（同 mod.rs:37-40 的串行） | join 并行 | safe |
| crates/semantic/src/ingest/autodiscover/probe.rs:181 | has_del=false 时拼出双空格 `FROM \`t\`  LIMIT 61`，测试（L277）把双空格钉成预期——瑕疵被测试固化 | 拼装时收掉空格，同步改测试 | test |
| crates/semantic/src/ingest/autodiscover/probe.rs:208 vs 219 | sample_values 把 `guard.max_rows`(200) 当 fetch 截断上限，sample_domain_values 传 DOMAIN_LIMIT：码型侧「SQL 上限 61」与「截断上限 200」不一致，guard 一旦调小（<61）哨兵静默失灵 | 码型侧也传 CODE_LIMIT（严格更紧，从不放宽） | safe |
| crates/semantic/src/ingest/autodiscover/probe.rs:208,219,223,246 | connector `effective_limits` 对生产能力连接把 max 钳到 50、timeout 钳到 2s（mysql.rs:1442-1448 自证）：探针目标若被配成生产能力，61 哨兵、2000 上限与 L223「单探针 10s」注释同时失效而本文件只字未提 | 注释写明该前提，或入口断言 capability 非生产 | safe |
| crates/semantic/src/ingest/autodiscover/probe.rs:254-260 | trim 后不去重：`' A'`/`'A'` 各留一份，下游 `distinct` 计数虚高（见 mod.rs:72） | 此处 dedup（对 best_dict_match 无影响，它内部 uniq） | test |
| crates/semantic/src/ingest/autodiscover/probe.rs:246 | `Duration::from_secs(10)` 裸字面量，注释（L223）给了语义但代码没给名字 | 提 `PROBE_TIMEOUT` const | safe |
| crates/semantic/src/ingest/autodiscover/probe.rs:70 | candidate_columns 返回 `Vec<(String,String,String)>` 裸三元组，消费处（mod.rs:46）靠位置解构 | 命名小结构 Candidate | safe |

## rerank.rs（13 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| rerank.rs:25-30 | `now()` 与 external_kb.rs:48-53 逐字重复（embed.rs 同款） | 上提 crate 内共享小工具 | safe |
| rerank.rs:12-15 | TIMEOUT/COOLDOWN 常量与 external_kb.rs:17-20 重复 | 共享常量 | safe |
| rerank.rs:34-41 | 直接调 `new` 时 base/model 空白不校验（只有 from_vars 拦），可造出 `"/rerank"` 这种 URL | `debug_assert!` 非空 | safe |
| rerank.rs:64 | 空 query 不拦（external_kb.rs:89-91 拦了），空白问句照样发给服务——口径不一 | 对齐加空 query 短路 | test |
| rerank.rs:71-75 | 每次调用新建 `reqwest::Client`（注释自认 ponytail）：热路径丢连接复用/TLS 会话 | 结构体内 OnceCell 缓存一个 Client | test |
| rerank.rs:75 | Client build 失败 `.ok()?` 静默 None | 失败时 debug 日志 | safe |
| rerank.rs:89 | 从不看 `resp.status()`：401/403（key 错）与形状不符不可区分，也不触发任何信号 | 非 2xx 时 warn 一次（带状态码） | safe |
| rerank.rs:93-97 | send 失败无日志，超时 vs 拒连现场分不清 | `tracing::debug!` 带 error | safe |
| rerank.rs:81 | `top_n = docs.len()` 无上限，调用方传几千篇就发巨型 body | clamp 或注释写明调用方契约 | test |
| rerank.rs:114 | `as_f64()? as f32` 放 NaN/Inf 进分数向量，污染下游排序 | `is_finite()` 过滤 | test |
| rerank.rs:106-121 | 形状不符静默 None，每问静默降级无痕迹 | debug 日志带原因（条数/下标/类型） | safe |
| rerank.rs:17-23 | 无 `Debug` 实现；直接 derive 会泄 api_key，不加又没法记日志 | 手写脱敏 `Debug` | safe |
| rerank.rs:163-228 | 测试桩 stub/find/content_len 与 external_kb.rs:238-306 近乎逐字重复 | 抽共享 test-util 模块 | safe |

## scripts/check-arch.ps1（12 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| scripts/check-arch.ps1:25,88 | 注释过滤 `^(//\ | \*\ | /\*)` 只认行首形态：块注释中间行若以非 `*` 开头、或代码行尾注释里含关键词，仍会命中——假红/漏网两种方向都没注释说明边界 |
| scripts/check-arch.ps1:41 | 模式 `sqlx::query` 是 `sqlx::query_as`/`query_scalar`/`query_with` 的前缀，实际把整族都禁了，标签却只写「不得 sqlx::query」——也许有意，但文案与规则宽度不符 | 标签改「不得 sqlx::query*」或模式收紧为 `sqlx::query\b\s*\(` | safe |
| scripts/check-arch.ps1:66 | `\bt_[a-z_]{3,}\b` 字符类无数字，含数字表名（如 `t_log2024`）漏检；`{3,}` 门槛漏掉 `t_ab` 类短名 | 字符类加 `0-9`，长度门槛降为 `{2,}` | test |
| scripts/check-arch.ps1:87 | 白名单正则未锚定文件名开头：未来的 `oauth.rs`、`xllm.rs` 会因结尾含 `auth.rs`/`llm.rs` 被意外豁免 | 改为 `\\(identity\ | wework\ |
| scripts/check-arch.ps1:87 | 白名单含 `identity`，但 crates/server/src 现有 33 个 .rs 里无 identity.rs——死条款或名单漂移 | 删掉或注释说明预留 | safe |
| scripts/check-arch.ps1:79-81 vs 87 | 注释说白名单是「auth.rs / llm.rs / wework.rs 三个」，正则却有 6 个备选；xcx_api 在 L80-81 补了理由，`identity`/`embed`（embed.rs 真实存在）为何算身份面无解释 | 注释补齐每个豁免文件的理由 | safe |
| scripts/check-arch.ps1:84-94 | ④ 的扫描逻辑与 Deny 函数（L23-25）是同一份代码的两份渲染，L75-79 注释自己记录过一次漂移事故——结构没变，下次改一处还会再漂 | 给 Deny 加 `-Whitelist` 参数复用同一条管道 | test |
| scripts/check-arch.ps1:23 | 每次 Deny 都重新 `Get-ChildItem -Recurse` 扫盘，13 条规则 13 次全量遍历 crates/（小性能） | 开头一次性收集 .rs 文件清单，Deny 复用 | safe |
| scripts/check-arch.ps1:109 | `foreach ($c in $order.Keys)` 遍历哈希表顺序不定，反向边报告顺序每次运行不同，diff 噪音 | `$order.Keys \ | Sort-Object` |
| scripts/check-arch.ps1:112 | 依赖解析模式 `^\s*dms-([a-z]+)\s*=` 的 `[a-z]+` 不含数字，未来 crate 名带数字会漏解析边（L123 的 ≥10 闸只能兜总量，兜不住单条） | 字符类改 `[a-z0-9]` | safe |
| scripts/check-arch.ps1:123 | edges 阈值 10 写死，但不像 EXPECT_RULES（L131-132）那样有注释交代「加 crate 要顺手改这里」 | 补同款签收注释 | safe |
| scripts/check-arch.ps1:141 | 注释引「H2 agent 实测」内部编号，仓内无对应文档可索引，后来人查不到出处 | 指向 docs/ 下具体文件或展开一句 | safe |

## skills_api.rs（12 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| skills_api.rs:56-61 | `db_err` 丢弃错误且**不打日志**：DB 故障完全无痕（ds_api::internal_err 同款有 warn，注释还自称「同一口径」） | 加 `tracing::warn!(error=%e, ...)` | safe |
| skills_api.rs:205-228,243-267 | create/update 的 409 预查与 INSERT/UPDATE 之间存在竞态：并发同名撞 UNIQUE 落 db_err 500 而非 409（注释 :71 承认预查「只是给人看的」，但未兜底） | 捕获 unique_violation（SQLSTATE 23505）映射 409 | test |
| skills_api.rs:225-228 | `INSERT ... RETURNING id` 用 `fetch_optional` + `ok_or(db_err("INSERT 未返回 id"))`；RETURNING 恒有行，ok_or 分支是死代码噪音 | 改 `fetch_one` | safe |
| skills_api.rs:341-343 | render 侧对 name 只 `sanitize_text`（剥控制字符）不剥 `"`：绕开 API 直写库的含引号 name 会撑破 `<untrusted_skill name="...">` 属性位；而注释 :457-458 声称「直写库的行也干净」，名不副实 | render 时对 name 一并替换 `"<>` | test |
| skills_api.rs:324-325 | `ENABLED_SQL` 字面量 `LIMIT 5` 与 `INJECT_MAX` 双写，仅靠测试对账 | 用 `concat!`/格式化由常量生成 SQL | safe |
| skills_api.rs:143-148 | `MAX_CONTENT_LEN + 1` 多取一字判超长的技巧无注释；且 `chars().count()` 二次遍历 | 加注释；或 sanitize 返回 `(String, truncated)` 一次完成 | safe |
| skills_api.rs:342 | `out.push_str(&format!(...))` 每包一个临时 String | `write!` 宏 | safe |
| skills_api.rs:51-53 vs ds_api.rs:27-29；93-97 vs ds_api.rs:53-57 | `err()` 与 `IdentQuery`/`DsQuery` 在两文件逐字重复 | 收敛到共享小模块 | safe |
| skills_api.rs:181 | list 把全量 content 一次性回给任意登录用户（读全认证是定案），但无 limit/分页，包多时响应膨胀且无注释说明刻意 | 加 `LIMIT`（如 200）或注释刻意全量 | safe |
| skills_api.rs:72-82 | DDL 无 `updated_at` 触发器，靠 update(:257)/toggle(:306) 手工 `now()`；未来新增 UPDATE 路径易漏 | 注释提醒或加触发器 | safe |
| skills_api.rs:173-193 | list 每行 `to_rfc3339()` 逐字段手工拼 JSON，8 元组解构冗长 | 定义 `SkillJson` serde 结构替代手工 json!（wire 形状不变） | safe |
| skills_api.rs:355-361 | `plan_prompt_suffix` 读库失败 warn 只带 `%e`，不带将跳过的包数/表上下文 | warn 补 `meta.skill` 表名上下文 | safe |

## seed_defs.rs（12 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| seed_defs.rs:36 | 注释称「口径与 `meta.term` 同名条目一字不差」，实际 42 行指标描述与 629 行术语定义并不相同（措辞/括号均异）——注释已漂 | 改注释为「两处口径同义、措辞各自维护」或真的对齐 | safe |
| seed_defs.rs:243,258,277,291,295,410,634,637 | 8 处硬编码 `ds_id='dms'` 字面量；同时 `upsert_metric`/`upsert_dimension`/`seed_terms` 的 INSERT 不写 ds_id 靠 DEFAULT——与文件内 DMS_DS_ID 绑定风格不一致，常量一旦变化半数语句静默失效 | 统一改为 `.bind(DMS_DS_ID)` | safe |
| seed_defs.rs:290-299 | 陈旧 code 清理循环每个 code 两次 DELETE round trip，且 element 与 metric 两步不在事务里，中途失败留孤儿 | 合并为 `= ANY($1)` 两次批量 DELETE，或包事务 | safe |
| seed_defs.rs:557-563 vs 597-603 | `("t_customer","customer_class",…)` 与 `("t_customer","customer_type",…)` 两组在 MAPS 里**逐字重复**出现两次，每次启动多打 14 条冗余 upsert | 删 597-603 的重复段，并给 MAPS 加 （表，列，名） 唯一性守卫测试 | safe |
| seed_defs.rs:531-540 vs 545-554 | 32 省行政区划 （名，码） 对在 `t_customer.province` 与 `t_sales_order.receiver_province` 两处逐字抄写，未来补码只改一处必漂 | 提取 `const PROVINCE_CODES: &[(&str,&str)]` 共用 | safe |
| seed_defs.rs:242-249 | `METRIC_POLICIES` 的 UPDATE 不查 `rows_affected`：code 与 METRICS 打漂时 version/allowed_dimensions 静默不生效，无任何告警 | 收集 0 行 code 并 `tracing::warn!` | safe |
| seed_defs.rs:17-172 vs 221-241 | 没有任何守卫保证「METRICS 里每个 code 在 METRIC_POLICIES 里有且仅有一条」（buyer_count 漏抄 OTHERS 的事故 839-841 行自己记录过同类漂） | 源码扫描测试：两集合 code 相等断言 | test |
| seed_defs.rs:325,436,647 | `aliases.iter().map(to_string).collect::<Vec<String>>()` 每条种子都重新分配；sqlx 可直接绑 `Vec<&str>` | 改绑 `Vec<&str>`，省每行 N 次 String 分配 | safe |
| seed_defs.rs:196-199 | `refund_ratio` 的 agg_expr 内嵌 `:begin`/`:end` 占位符，是全文件唯一自带占位符的指标（其余靠 time_col 由装配器补）；替换方是否覆盖此路径无本地断言 | 加测试断言该 expr 经装配后占位符被替换 | test |
| seed_defs.rs:656-681 | 关于裸「余额」与「码值过滤不写中文名」两大段 `///` 文档挂在 `code_filters_never_use_chinese_names`(705) 头上，但前者讲的是 `no_bare_balance_alias`(768)——文档错位 | 把 656-664 移到 768 行前 | safe |
| seed_defs.rs:823-848 | 测试用 `OTHERS` 手抄指标名+别名，839-841 行注释自承 buyer_count 当年漏抄导致碰撞断言空转；ACTIVE_SKU/GIFT 也是抄本 | 加一条源码扫描测试：METRICS 中每个 name 必在 OTHERS/ACTIVE_SKU/GIFT 之一 | test |
| seed_defs.rs:55 vs seed.rs:103 | 指标 `market_expense` 描述明令「禁止使用旧 ODS 合计表」，而 `seed_warns` 里 `t_market_marketing_expense` 的 warn 仍指示「泛指市场费用一律用 t_market_total_expense」——两条资产口径互相矛盾，LLM 同时读到 | 与业主确认后更新 warn 文案指向 ADS 宽表 | test |

## tabular.rs（12 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| tabular.rs:53 vs 135 | 通道①跳过谓词是「表头空 **且** 行空」，通道②是「表头全空白」（有数据行也跳）：一张「无表头但有 500 行」的 sheet 会进检索 markdown 却无表可问，且不进 `skipped`，两通道口径互相矛盾 | 统一谓词（建议都以表头全空白为准），或注释明说差异是有意的 | test |
| tabular.rs:97-107 | `materialize` 跨 create schema/建表/灌数无事务无回滚：第 3 个 sheet 灌数失败，前两张表 + schema 成孤儿（上传失败文档不登记，`drop_source` 永远不会被调） | 错误分支 best-effort 调 `drop_upload_schema` 清场再返回 Err | test |
| tabular.rs:142-144 | 全部 sheet 被跳过时 `Err` 里不带 `skipped` 名单，用户不知道是哪些 sheet 空 | 错误文案附上跳过的 sheet 名（现有测试用 `contains` 匹配，追加不破坏） | test |
| tabular.rs:170-175 | `spec_of` 对每列调一次 `samples()`，每列都重扫前 200 行：200 列 × 200 行 = 4 万次迭代，可一次转置 | 在 `plan`/`spec_of` 外一次性把前 SAMPLE_ROWS 行转置成列向量再传入 | safe |
| tabular.rs:74-77 | `upload_schema_of_ds("upload_")` 剥前缀后 doc_id 为空串，仍返回 `Some("up_")` 之类的退化 schema | 空 remainder 直接返回 None | test |
| tabular.rs:61-63 | `upload_ds_id` 依赖「doc_id 是 uuid（36 字符）」这一隐式假设来满足 `valid_ds_id` ≤64，假设只写在测试里 | 函数文档加一行「调用方保证 doc_id 为系统生成 uuid」或 `debug_assert!(doc_id.len() <= 57)` | safe |
| tabular.rs:48-51 | 注释引用的「`embed_service::_sheet` 的红字」是跨语言外部锚点，Python 侧改名这里就成指路错误 | 注释里补文件名全路径（`docker/parser` 或 tools 下的具体文件），降低漂移成本 | safe |
| tabular.rs:101 | 多 sheet 顺序建表灌数，sheet 间无依赖可流水线化，但当前实现简单直白 | 仅注释说明「顺序是有意的（失败定位到单个 sheet）」即可，不必改代码 | safe |
| tabular.rs:135 | 空表头判定用 `all()`，空 Vec 恒真——语义正确但读的人要想一秒 | 抽个命名谓词 `fn header_blank(s: &Sheet) -> bool` 并复用到 sheet_blocks（见第一条统一谓词） | test |
| tabular.rs:150-163 | 行/列两个上限检查文案风格不统一（行文案带「请拆分后重传（不做截断）」，列文案没有） | 列文案补齐同样的指引后缀 | safe |
| tabular.rs:186 | `samples` 注释说「空白值由 `infer_col_type` 自己滤」，但没钉测试；若 connector 那边行为变了这里静默漂移 | 在本文件加一条「全空白列 → Text」的接线测试 | test |
| tabular.rs:306 | 注释说「`Plan` 不实现 Debug」，属「注释描述代码属性」类，将来有人给 Plan 加 Debug 注释就撒谎 | 改成「`plan_err` 用 let-else 取错误分支」这类不随 derive 漂移的表述 | safe |

## crates/kernel/src/nl/lexicon.rs（12 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/kernel/src/nl/lexicon.rs:38 | `"top","TOP"` 两个形态但没有混合大小写 "Top"；`has_residue_with` 不 lowercase → "Top5" 残留字母判实义残留回落 LLM，与 detect_top_n（先 lowercase）口径不一 | 守卫侧对 ASCII 统一小写化，或补形态 | test |
| crates/kernel/src/nl/lexicon.rs:58 | 数字虚词有「一..十、百」独缺「两」：recent_n 认「两」（「近两个月」解析成功），残留守卫却留下「两」→ 判残留回落 LLM，两条路自相矛盾 | 补「两」（实体名风险极低） | test |
| crates/kernel/src/nl/lexicon.rs:44 | 有「最高/最多/最少/最大/最小」独缺「最好」，而 detect_top_n(time.rs:57) 认「最好」：「卖得最好的10个商品」TopN 认得出、残留守卫却留「最好」回落 LLM | 补「最好」 | test |
| crates/kernel/src/nl/lexicon.rs:26 | 词序「年」(L26) 先于「至今」(L26 末）：「年初至今的销量」剥完「年」→「初至今」→ 剥「至今」→ 剩「初」→ 判残留回落；window_includes_today(time.rs:516) 却认「年初至今」 | 「年初至今」加进表并排在「年」之前 | test |
| crates/kernel/src/nl/lexicon.rs:37 | `"查"` 排在 `"查询"` 之前 → 「查询」是死条目（replace 顺序承重，「查」先剥后「查询」永不出现） | 删掉死条目或换序，连带改 L93 计数 | safe |
| crates/kernel/src/nl/lexicon.rs:11 | 同理「上个月」在「上月」之后是死条目（残留「个」由 L57 兜住）；L142 的测试把无害顺序钉成「必须」 | 注释承认冗余是防御性的，或删条目 | safe |
| crates/kernel/src/nl/lexicon.rs:63-66 | `FOLLOWUP_MARKS` **零消费者**（全仓 grep 仅自身+测试）；真正的 is_followup 在 agent/ask.rs:1205 内联逐字相同的一份，注释引用的 `server/src/pipeline.rs:504-507` 已不存在——「单一事实源」没人吃 | ask.rs 接线用 kernel 常量，或删常量 | test |
| crates/kernel/src/nl/lexicon.rs:70-72 | `TIME_TOKENS` 同样零消费者；agent 侧 triage.rs:69 与 cache.rs:107 各抄一份（两处注释都自称单一事实源），三份当前逐字相同——漂移条件已具备 | 三处收敛到 kernel 常量 | test |
| crates/kernel/src/nl/lexicon.rs:70-72 | TIME_TOKENS 缺「今日/昨日/这周/当月/本年/年初至今/近…」（STRIP_WORDS 与 window_includes_today 都认）：「近7天」vs「近30天」时间 token 集同为空集，缓存护栏这一层拦不住不同窗 | 补齐同义词，「近 N」形态单独评估 | test |
| crates/kernel/src/nl/lexicon.rs:79-81 | SENSITIVE_COLS 无 "pwd"，而 qalog.rs:76 的 SENSITIVE_KEYS 有——列名恰叫 `pwd` 的表三处防线（schema 剔除/SQL 闸门/结果置空）全漏 | 补 "pwd" 并两文件键表互相对照 | test |
| crates/kernel/src/nl/lexicon.rs:93 | `// 上一版 80` 是考古信息，随每次加词腐化 | 删或改为「计数漂移锁，加词同改」 | safe |
| crates/kernel/src/nl/lexicon.rs:135 | 测试用「不含元/件/装」当实体名风险判据，新实义字根不会触发，判据脆弱 | 改为对照注册表真实字根集合（或注释承认弱判据） | safe |

## scripts/serve.ps1（11 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| scripts/serve.ps1:25 | 只读 `settings.docker.json`，缺文件时 `Get-Content` 抛裸错；run.ps1:8-14 有回退与友好文案，两处不一致 | 复用 run.ps1 的回退 + 友好 throw | safe |
| scripts/serve.ps1:26 | 正则匹配前未 `Trim()`，settings 里 service_url 带尾随空格/换行会静默跳过 ensure-services——首次上传就撞 L23-24 注释要防的 300s 熔断 | 匹配前 `.Trim()` | safe |
| scripts/serve.ps1:26-28 | 只认 `http://host.docker.internal:8078` 才带起依赖链，其它配置静默跳过且无一句提示 | else 分支打一行「service_url 非本机解析链，跳过依赖检查」 | safe |
| scripts/serve.ps1:47 | `D:\kbdata` 硬编码 + 未校验仓库盘符：L44-46 注释自己强调「必须是 D 盘…换成别的盘符这个巧合就断」，但脚本对仓库克隆到 C: 无任何守卫，静默断裂 | 校验 `$repo` 盘符为 D:，否则 throw 并指到这段注释 | safe |
| scripts/serve.ps1:80 | `-split ' '` 对引号无处理，用户自然写法 `-Cmd 'xxx "带空格 参数"'` 被静默错切；头部注释（L8-9）说了限制，但运行时不给任何提示 | split 前检测 `"` 并打警告指向头部注释 | safe |
| scripts/serve.ps1:80 | 一次性 -Cmd 路径不检查镜像存在，未 build 时 docker 报「Unable to find image」并尝试拉远端，错误不指向「先跑 -Build」 | `docker image inspect dms-ai-server` 预检，缺则提示 -Build | safe |
| scripts/serve.ps1:91 | `docker run -d` 无 `--restart`，而 PG compose 有 `restart: unless-stopped`（docker/age/docker-compose.yml:5）——机器重启后 API 静默不在，策略不一致 | 加 `--restart unless-stopped` 或注释说明刻意不加 | test |
| scripts/serve.ps1:94 | 注释写「实测约 28s；原 21s 窗口」却不写当前窗口 90×700ms≈63s，读者要自己算 | 注释补「当前窗口约 63s」 | safe |
| scripts/serve.ps1:99-100 | health 成功后裸打 JSON 无标签、无收尾信息（服务地址、日志查看命令），与 parser.ps1:137 同款裸输出 | 加「[ok] http://127.0.0.1:8100 就绪」一行 | safe |
| scripts/serve.ps1:103 | 失败文案「health 90 次未通」以次数为单位，L94 注释以秒为单位，口径不一；运维要心算 90×0.7 | 改「约 63s 未通」 | safe |
| scripts/serve.ps1:84 | `docker rm -f` 强杀在跑容器无任何提示，终端上看不出「旧容器被替换」这件事 | 打一行「替换旧容器 dms-ai-server」 | safe |

## crates/agent/src/answerers/graph.rs（11 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/agent/src/answerers/graph.rs:26 | `Relation` 只 derive `PartialEq` 没 derive `Eq`（String 载荷完全可 Eq） | 补 `Eq` | safe |
| crates/agent/src/answerers/graph.rs:87 | `!cx.source.is_warehouse()` 复查是死条件：cx.source 跨 await 不变，accept（70 行）已强制 warehouse——TOCTOU 防的是快照代次，不是数据源 | 删该合取项或注释「防御性复查」 | safe |
| crates/agent/src/answerers/graph.rs:99-101 | 三条图查询 `.ok()?` 把 AGE 错误零留痕吞掉回落 LLM——hits.rs:125-132 为同款静默付过一次账（5 轮回归失败无言） | `.map_err( | e |
| crates/agent/src/answerers/graph.rs:118,176,186 | `resolved_buyers` 内 resolve_entities/buyers_filtered 两处 `.ok()?` 同样静默吞错 | 同上补 warn | safe |
| crates/agent/src/answerers/graph.rs:129-138 | `rows_data.iter().map(... g.code.clone() ...)` 逐字段克隆，其后 rows_data 不再使用 | `into_iter()` 直接 move code/name | safe |
| crates/agent/src/answerers/graph.rs:135 | `Value::from(format!("{:.2}", g.amount))` 把购买额存成 JSON **字符串**：前端排序/合计/CSV 全变文本语义，present 也认不出数值列 | 改数值（如 `(amount*100.).round()/100.`），配视图/回归测试 | test |
| crates/agent/src/answerers/graph.rs:144,154-155 | `truncated: false` 恒写死且注释自圆「到不了 MAX_ROWS」；但 Cypher `LIMIT 50` 恰好 50 行时结果本身边就被截了，用户看不到任何提示 | `row_count >= 50` 时置 truncated/写 truncation_note，配测试 | test |
| crates/agent/src/answerers/graph.rs:224 | `covered` 直接对窗口汉字数求和：若 `resolve_entities` 产出**重叠**窗口会重复计数、虚增覆盖率，覆盖率判据（本文件唯一的静默错答防线）被绕 | 先按 start 排序去重/合并重叠窗口再求和，配重叠窗口用例 | test |
| crates/agent/src/answerers/graph.rs:238-268 | `unsupported` 数组每次调用重建，且 accept(72)+answer(78) 每问至少调两次 | 提为模块级 `const UNSUPPORTED: &[&str]` | safe |
| crates/agent/src/answerers/graph.rs:239-252 | 黑名单含被短词子串覆盖的死条目：「商品分类」⊂「分类」、「商品类型/客户类型」⊂「类型」、「商品大类」⊂「大类」、「商品小类」⊂「小类」、「销售区域」⊂「区域」 | 删死条目（行为逐字节不变）或注释其存在理由 | safe |
| crates/agent/src/answerers/graph.rs:279 | `constraint_hanzi_count` 两次 `replace` 两次分配，且与 hanzi_count 组合每次 answer 至少跑两遍 | 合并为单遍 chars 过滤（跳过「省区/省份」子串）或注释「量小不优化」 | safe |

## crates/semantic/src/ingest/autodiscover/mod.rs（11 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/semantic/src/ingest/autodiscover/mod.rs:35 | 注释「读写都固定在 'dms' 那一格（与 `sync_schema` 同）」不准：sync_schema 的 ds 由调用方传（schema_sync.rs:11-12 自述），并不固定 'dms' | 删「与 sync_schema 同」或改为「sync_schema 的 DMS 调用点同」 | safe |
| crates/semantic/src/ingest/autodiscover/mod.rs:36 vs 109,120 | 外层 `let ds = DMS_DS_ID`，而 discover_domain_values 内部两处又直接写 DMS_DS_ID——两处事实源，改 ds 语义时只改一半 | 给 discover_domain_values 加 ds 参数 | safe |
| crates/semantic/src/ingest/autodiscover/mod.rs:37-40 | 四条加载（1×MySQL + 3×PG）互不依赖却串行 await，启动白白相加四段延迟 | `tokio::try_join!` 并行 | safe |
| crates/semantic/src/ingest/autodiscover/mod.rs:46-75 | 候选循环全串行：单探针最坏 10s × N 候选，且无并发上限也无进度日志，跑半小时外面一动不动 | 小并发 buffer_unordered，或至少每 N 条 info! 进度 | test |
| crates/semantic/src/ingest/autodiscover/mod.rs:74 | 单次 register_match 失败 `?` 中止整轮：前面已注册的保留、后面候选全丢，输出 JSON 里也没有 failed 计数 | warn 继续 + `failed` 计数进输出 JSON | test |
| crates/semantic/src/ingest/autodiscover/mod.rs:47-49 | is_backup_table/is_sensitive_col 跳过的候选不计数，输出 JSON 缺 skipped_backup/skipped_sensitive，对账时 candidates 与 probed 的差解释不了 | 两个计数进输出 JSON | safe |
| crates/semantic/src/ingest/autodiscover/mod.rs:55-57 | sample_values 返 None 的两种原因（闸门拒/抽样失败）只在 warn 日志里，JSON 无 probe_failed 计数 | 计数进输出 JSON | safe |
| crates/semantic/src/ingest/autodiscover/mod.rs:55-58 | `Some(vec![])`（空表/全 NULL 列）也计入 probed，口径含空抽样 | 空集不计 probed 或注释写明口径 | safe |
| crates/semantic/src/ingest/autodiscover/mod.rs:72 | `distinct: values.len()` 是「抽样行数」不是「不同值个数」（trim 后可撞名，register.rs:23-24 注释自称不同值） | dedup 后取 len，或改注释为「抽样行数」 | safe |
| crates/semantic/src/ingest/autodiscover/mod.rs:77-80 | discover_domain_values 或 sync_elements 失败 = 整轮 Err，前面字典段的全部注册结果 JSON 一起丢（运维看不到部分成果） | 分段容错，先产出已得 JSON 再跑收尾 | test |
| crates/semantic/src/ingest/autodiscover/mod.rs:26 vs probe.rs:76 | 码型后缀清单 `*_code/_type/_status/_class/_mode/_way/_level` 在 doc 注释与正则里各写一份，改一处漏一处必漂 | 注释改为指向 probe.rs 正则，不复制清单 | safe |

## crates/semantic/src/ingest/autodiscover/match_dict.rs（11 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/semantic/src/ingest/autodiscover/match_dict.rs:31 | `kc.to_lowercase()` 每候选列 × 每字典重复分配（一轮数百候选 × 数十字典） | 调用方一次性构建小写键视图，本轮内 dicts 不变 | safe |
| crates/semantic/src/ingest/autodiscover/match_dict.rs:32-35 | filter 里 `dicts.get(*kc)` 对已知来自 keys() 的 kc 多一次哈希查找 | 改 `dicts.iter()` 同时拿键值 | safe |
| crates/semantic/src/ingest/autodiscover/match_dict.rs:41,56-62 | HashMap 迭代序随机 + `(cov,hit) >` 严格大于：同 cov 同 hit 的两个字典谁中看本次运行的随机序，注册结果跨轮不可复现 | 候选按 key 排序，或 tie-break 比较 key | test |
| crates/semantic/src/ingest/autodiscover/match_dict.rs:46 | 每候选列对每字典重建 codes HashSet（cands×dicts×pairs 次哈希插入） | 构建一次 DictIndex（key→码集）跨候选复用 | safe |
| crates/semantic/src/ingest/autodiscover/match_dict.rs:61 | 每次刷新 best 都 `pairs.clone()` 整份字典码表，一轮克隆多次 | best 存引用，出循环后克隆一次 | safe |
| crates/semantic/src/ingest/autodiscover/match_dict.rs:74 | `has_common_3gram(&c,&d) \ | \ | has_common_3gram(&d,&c)`：「存在公共 3-gram」数学上对称，两个方向恒等，第二次调用纯浪费；注释「双向包含判定」是对称关系的误述 |
| crates/semantic/src/ingest/autodiscover/match_dict.rs:71-73 | 3-gram 判定 O( | a | × |
| crates/semantic/src/ingest/autodiscover/match_dict.rs:23,49,52,74 | 2/60/0.8/8/3 五个判据魔数散落函数体，doc 注释（L12-15）与代码各自写数 | 提名为 const，注释引常量 | safe |
| crates/semantic/src/ingest/autodiscover/match_dict.rs:31 | `kc.len() >= 4` 按字节：含 CJK 的 key_code 四个字节可能只有 1-2 字符，短键溜进点名闸 | 改 `.chars().count()` 或注释注明键按 ASCII 假设 | safe |
| crates/semantic/src/ingest/autodiscover/match_dict.rs:34 vs 31 | 同一条点名闸两种大小写口径：key 点名 lowercase 后比，name 点名 `col_comment.contains(kn)` 大小写敏感——字典名含 ASCII（如「WMS状态」对注释「wms状态」）漏点名 | 统一大小写口径 | test |
| crates/semantic/src/ingest/autodiscover/match_dict.rs:16-20 | 返回 4 元组裸类型，调用处靠位置解（mod.rs:59） | type alias 或小结构 | safe |

## crates/kernel/src/qalog.rs（11 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/kernel/src/qalog.rs:35-37 | `clip` 对短串也走 `chars().take().collect()` 一次分配 | 加快路径 `if s.len() <= CLIP_CHARS { return s.to_string() }`（字节 ≤2000 则字符必 ≤2000） | safe |
| crates/kernel/src/qalog.rs:55 | userinfo 判据排除 `' '/'\t'` 但漏 `'\n'/'\r'`：多行错误文案里 `@` 在下一行时会跨行误剥正常文本 | `matches!(...)` 改 `c.is_whitespace()` 或补 `\n\r` | test |
| crates/kernel/src/qalog.rs:75-76 | SENSITIVE_KEYS 缺 "passphrase"/"authorization"/"cookie"（Set-Cookie 错误文案同样可能回带凭据） | 按本仓实际错误源补键 | test |
| crates/kernel/src/qalog.rs:80-83 | 键名回扫只认 `[A-Za-z0-9_]`：`api-key=...`/`x.api.key=...` 被截成 "key"，不在表 → 漏剥 | 键名字符集补 `-` `.`，或补归一化后的键 | test |
| crates/kernel/src/qalog.rs:87 | 值终止符缺 `,`：`"password=abc,host=db"`（无空白）把 `abc,host=db` 整段剥成 `***`——安全方向但掩盖排障信息 | 终止符补 `,` | test |
| crates/kernel/src/qalog.rs:105-107 | `msg.to_ascii_lowercase()` 调两次 → 两次全串分配 | 提一次局部变量复用 | safe |
| crates/kernel/src/qalog.rs:106-107 | 缺 "deadline exceeded"（gRPC 形态）；若将来引入 tonic 类客户端会误判成 failed | 注释声明当前只覆盖 reqwest/自研两类来源，或补词 | safe |
| crates/kernel/src/qalog.rs:12-16 | INSERT_SQL 内嵌换行+缩进直接进 SQL 文本，慢查询日志里全是噪声空白 | `concat!` 拼单行，字节即最终 SQL | safe |
| crates/kernel/src/qalog.rs:25 | 「audit 白名单三路同改」只有注释提醒，无编译期/测试期保障 | datamap_api 侧加测试直接断言白名单 == qalog 四常量 | test |
| crates/kernel/src/qalog.rs:77 | `String::with_capacity(s.len())` 不是上界：`pwd=1` → `pwd=***` 反而变长（无 bug，push_str 自扩容，但容量假设与直觉相反） | 注释一句或不管；标注即可 | safe |
| crates/kernel/src/qalog.rs:54 | 多 `@` 形态（`user@h1@h2`）只剥到第一个 `@`，残留第二段——罕见且方向安全，但无注释 | 注释声明单 `@` 假设 | safe |

## rules.rs（11 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| rules.rs:42 | `WHERE 1 = 1 AND ds_id IN (...)` 的 `1 = 1` 是动态拼串时代残留的恒真谓词（现有测试只断言 contains ds 谓词，删除不红） | 删 `1 = 1 AND ` | safe |
| rules.rs:41-42 + 243-246 | `LOAD` 无 `ORDER BY`，而 collect 进 HashMap 是后写覆盖先写：同一表同时存在 ds 专属行与 `'*'` 行时，生效行取决于 PG 返回序——权限档案不确定 | `ORDER BY CASE WHEN ds_id='*' THEN 0 ELSE 1 END`（专属行后写胜出） | test |
| rules.rs:185 | 未知 mode 的 warn 用 `{other}` 不带 `:?`，与同函数 L144/156 的 `{other:?}` 不一致；mode 含控制字符时日志注污 | 统一 `{:?}` | safe |
| rules.rs:170-174 | via 臂先 `clone()` 三个 Option 再判 None：缺列时另外两个 clone 白做 | `match (self.via_table.as_deref(), ...)` 后 `to_string()` | safe |
| rules.rs:248 | `load_rules` 在 `n==0`（ds 名写错等运维事故）时静默 install 空表 → 全表 fail-closed 且无任何日志 | `if n == 0 { tracing::warn!(...) }` | safe |
| rules.rs:33-35 | `install` 热更新档案这一运维事件完全无日志，排障无法确认档案何时被换 | `tracing::info!("权限档案热更新: {} 条", rules.len())` | safe |
| rules.rs:214-237 | `seed_rules` 的 DELETE_RETIRED + 39 条 UPSERT 不在事务里，中途失败留混合态（幂等重跑可自愈但未注明） | 包事务，或在 doc 注明「失败重跑即自愈」 | test |
| rules.rs:220 | `for (t, rule) in builtin_rules()` HashMap 随机序 upsert，执行/日志顺序每轮不同 | `sort_unstable_by_key( | (t,_) |
| rules.rs:91 | `BindingRow::of` 默认 `customer_kind: Some("codes")` 对 Global/Via 臂也原样落库，种子行带误导性列值（读侧忽略所以无行为问题） | 各臂显式 `r.customer_kind = None` | test |
| rules.rs:243-246 | `rows.iter()` + `r.table_name.clone()`：`to_rule(&self)` 内部再 clone 各列，每行多次可省分配 | `to_rule(self) -> Option<(String, TableRule)>` 消费行 | safe |
| rules.rs:3 | 头注释「相对 server/src/inject.rs 的改动」——该文件已删除（builtin.rs:6 已正确标注「已删除」），此处未标 | 补「（已删除）」 | safe |

## scripts/ensure-services.ps1（10 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| scripts/ensure-services.ps1:19 | embed 启动未传 host 参数 → 绑 127.0.0.1（embed_service.py:1241 默认），parser 容器 `/embed` 透传 `host.docker.internal:8077` 必 503；embed_service.py:1690 已支持 host 参数但这条链路没用它 | 给 serve 追加 host 实参（如 172.17.0.1，embed_service.py:1243 注释推荐值） | test |
| scripts/ensure-services.ps1:36,45 | 健康判据 `$r.parse_ok.text` 恒真：text 家族的 probe 是 `_cap_ok`（embed_service.py:741-748 恒返回可用），等于只探测活不探能力，docx/pdf 全坏也报 up——正是本仓反复批判的「恒真判据」形态 | 改查 `parse_ok.docx`/`.pdf` 或 parse_caps 逐扩展名 | test |
| scripts/ensure-services.ps1:31 | throw 前不把 `$env:TEMP\dms-ai-embed.err.log` 尾部打出来，用户要自己去翻临时目录 | throw 前 `Get-Content -Tail 20` 该 err log | safe |
| scripts/ensure-services.ps1:32 | embed 本来就在跑也打「embed: up :8077」，与「本次新拉起」无区分，日志说不清发生了什么 | 已在跑打「已在运行」，新拉起打「已启动」 | safe |
| scripts/ensure-services.ps1:13,23 | 文案「模型加载约 5~15s」，等待窗口 40×500ms=20s，余量仅 5s，慢盘冷加载会误 throw | 窗口放宽到 40s 或文案与窗口对齐 | safe |
| scripts/ensure-services.ps1:40,43 | 用 `pwsh -NoProfile -File` 起子进程调 parser.ps1——Windows PowerShell 5.1 环境无 pwsh 时直接失败；run.ps1:32/35 调兄弟脚本都是 `& "$PSScriptRoot\x.ps1"` 同会话，两种风格不一致 | 改为 `& "$PSScriptRoot\parser.ps1" build` 同会话调用 | safe |
| scripts/ensure-services.ps1:38 | docker daemon 未运行时 `docker image inspect` 非零 → 走 build 分支，报「镜像构建失败」而非「docker 未运行」，文案误导 | 先探测 daemon | safe |
| scripts/ensure-services.ps1:14-18 | venv 缺失时回退裸 `'python'`，PATH 无 python 时 `Start-Process` 抛裸错无提示 | 回退前 `Get-Command python` 探测，throw 明文 | safe |
| scripts/ensure-services.ps1:21-22 | 日志固定写 `$env:TEMP` 固定文件名，多实例/多用户互相覆盖，排障时看到的是别人的日志 | 文件名带日期或写仓内 target/ 下 | safe |
| scripts/ensure-services.ps1:48 | 无显式 `exit 0`，调用方 run.ps1:33 靠 LASTEXITCODE 判成败会读到残留值（见 run.ps1:33 条） | 末尾 `exit 0` | safe |

## docs/DEPLOY.md（10 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| docs/DEPLOY.md:23 | 「必填…`llm_providers`（模型供应商 key）」错误：key 实际填在 `llm_keys`（settings.example.json:27）；`llm_providers` 是可选自定义供应商连接形状且非必填——照抄会填错键并触发启动硬失败 | 改为 `llm_keys` | safe |
| docs/DEPLOY.md:23 | 必填清单含 `service_url`，但它有默认 `http://127.0.0.1:8077`（db.rs:372-374），裸机同机部署不必填 | 标注「仅容器/跨机时需改」 | safe |
| docs/DEPLOY.md:25 | 「AES-GCM」与 CONFIG.md:21 的「AES-256-GCM」写法不一 | 统一为 AES-256-GCM | safe |
| docs/DEPLOY.md:34 | 裸机指引 embed 起在 `serve 8078`，与默认 `service_url` :8077（embed_service.py:9、db.rs:373）不一致——照抄则 embed/解析全链打不通 | 改为 8077，或注明须同步改 `service_url` | safe |
| docs/DEPLOY.md:34 | 「裸机 Linux」小节首句仍是 `docker compose`，小节名与内容不符 | 改为「依赖容器化、API 裸机」之类 | safe |
| docs/DEPLOY.md:36 | 只警告 AGE 在非默认库缺失，实际 vector/pg_trgm 由同一初始化脚本也只建在默认库（docker/age/init/01-extensions.sql:2-4） | 三个扩展一起提醒 | safe |
| docs/DEPLOY.md:40 | 硬编码计数（码值 938 行/auto 维度 70/软删 35/样例 172/教训 18）仓内无法核对（仓内 registry_snapshot.json 是每表 3 行的样例）且随现网持续漂移 | 加「数字为撰写时口径，以现网导出为准」或去具体数 | safe |
| docs/DEPLOY.md:49 | 「导入后 10 分钟内回填」不准：向量自愈是「启动即跑一轮 + 每 10 分钟一轮」（embed_fill.rs:1、INTERVAL=600s:24） | 改为「启动即一轮，之后每 10 分钟」 | safe |
| docs/DEPLOY.md:64 | 验收清单漏 `mysql.session_read_only:true`——它是 `ok` 的组成项（main.rs:2308-2311），只读校验挂了 `connected` 仍为 true，会误判通过 | 补上该字段 | safe |
| docs/DEPLOY.md:67 | 「77 题」与实际题数不符：tools/regression_cases.json 的 `cases` 实为 76 | 改 76 或不写死题数 | safe |

## docker/age/docker-compose.yml（10 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| docker/age/docker-compose.yml:12 | **潜在数据丢失 bug**：挂的是 `/var/lib/postgresql`，而 postgres 系镜像 PGDATA=`/var/lib/postgresql/data` 且声明了 VOLUME——挂父目录不覆盖子路径 VOLUME，Docker 会为 data 另建匿名卷；数据实际落匿名卷，`docker compose down` 默认删匿名卷 = 元数据库全丢，named volume 一直是空壳 | 改为 `dms_ai_pgdata:/var/lib/postgresql/data`（改后需重建验证 init 重跑） | test |
| docker/age/docker-compose.yml:1-16 | 行尾混合 CRLF/LF（实测 L1-9、14-16 带 \r，L10-13 不带） | 统一 LF | safe |
| docker/age/docker-compose.yml:全文 | 无 healthcheck：run.ps1:16 只按容器名判「PG 已运行」，initdb/崩溃恢复期也算通过，后续步骤裸奔 | 加 `healthcheck: test: ["CMD-SHELL","pg_isready -U postgres -d dms_ai"]` | safe |
| docker/age/docker-compose.yml:3 | 无 `image:` 名：build 产物名由 project 目录名派生（`age-dms-ai-pg` 之类），`docker images` 里认不出归属 | 加 `image: dms-ai-pg` | safe |
| docker/age/docker-compose.yml:7 | `${DMS_AI_PG_PASSWORD:?set DMS_AI_PG_PASSWORD}` 报错文案只有变量名——手工 `docker compose up` 的人不知道去哪设（run.ps1:22 是自动注入的） | 文案改为「set DMS_AI_PG_PASSWORD（或由 scripts/run.ps1 自动注入）」 | safe |
| docker/age/docker-compose.yml:6-8 | 无显式 `POSTGRES_USER`：默认 postgres，settings 的 pg_url 用户名必须恰好是 postgres；DEPLOY.md:36 已踩过「扩展只建在默认库」的同类错位 | 显式写出 `POSTGRES_USER: postgres` 或注释约束 | safe |
| docker/age/docker-compose.yml:10 | 15433 这个偏移端口无理由注释（为何不是 5432——避让本机已有 PG？） | 注释一句 | safe |
| docker/age/docker-compose.yml:4 | `container_name` 写死，`docker compose -p` 起第二套实例直接撞名 | 注释说明有意写死（单实例部署）或去掉 | safe |
| docker/age/docker-compose.yml:全文 | DB 容器无 `logging` 限制，json-file 日志无界增长 | 加 `logging.options.max-size/max-file` | safe |
| docker/age/docker-compose.yml:全文 | 无 `stop_grace_period`：默认 10s 后 SIGKILL，PG 大事务停机可能被硬切 | `stop_grace_period: 60s` | safe |

## vision_api.rs（10 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| vision_api.rs:51-53 | `load_principal` 失败 `map_err( | _ | ...)` 丢弃原因且无 warn；ds_api::identity_err 同款场景有 warn 留痕 |
| vision_api.rs:51-55 | 先 `load_principal`（一次库查）后做廉价的 prompt 长度校验，失败请求也打一次库 | 长度校验前置（错误优先级变化：未认证+超长时先回 400） | test |
| vision_api.rs:54 | 只限上限不限空白：空/全空白 prompt 直接送模型 | `trim().is_empty()` 拒 400 | test |
| vision_api.rs:54-55 | `20_000` 字面量与文案「20000 字节」双写；且按字节与其他面（skills 按字符）口径不同无注释 | 提常量 + 注释说明按字节的理由 | safe |
| vision_api.rs:16,58-61 | `image_url` 无入口长度预检，16MB data URI 全量进内存后才在 llm 层拒 | 入口粗筛（如 `len() > 24MB` 直接 413） | test |
| vision_api.rs:48 | `strip_prefix("Bearer ")` 大小写敏感，`bearer ` 直接 401；RFC 6750 scheme 不敏感 | 大小写不敏感比较（先与 auth::resolve 口径对齐） | test |
| vision_api.rs:72 | `total_tokens` 本地 `saturating_add`；上游 usage 常自带 total（图像 token 可能只计入 total），llm.rs:570 只读 prompt/completion，总数可能低估 | 注释「刻意本地加和」或透传上游 total | safe |
| vision_api.rs:39-43 | 全仓最贵端点之一却无限流（评审批已给其他面加限流） | 复用现有限流件 | test |
| vision_api.rs:1 | 模块头英文单行，与全仓中文模块头（含纪律/对应物说明）风格不一 | 补中文模块头 | safe |
| vision_api.rs:149-163 | 测试钉了「不许 login_name 回退」，但没钉 prompt 20_000 上限闸的存在 | 测试补上限断言 | safe |

## crates/semantic/src/ingest/autodiscover/register.rs（10 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/semantic/src/ingest/autodiscover/register.rs:34-56 | 每对 (code,name) 一条 INSERT：字典 60 码 = 60 次 round trip，遇大字典（数百码）一轮上千次 | 多行 VALUES 或 unnest 批量 | test |
| crates/semantic/src/ingest/autodiscover/register.rs:34-56 vs 80-88 | dict 路径只 upsert 不删旧行：字典删值/改名后旧 name 行永留 value_map，与 mod.rs:25「字典变了重跑即自适应」的承诺不符（domain 路径就有先 DELETE） | 注册前按 (ds，表，列） 清掉 origin=dict 旧行再插 | test |
| crates/semantic/src/ingest/autodiscover/register.rs:58 | `pairs.len() <= 60` 不注册维度时，输出 JSON（L61-64）无「维度未注册」标记，运维以为全注册了 | JSON 加 `dimension_registered: bool` | safe |
| crates/semantic/src/ingest/autodiscover/register.rs:80-105 | DELETE + 逐条 INSERT 不在事务里：中途崩溃 = 该 （表，列） 词典残缺到下次重跑；两轮并发交错更乱 | 包事务 | test |
| crates/semantic/src/ingest/autodiscover/register.rs:68,106 | doc 注释「返回写入条数」与实际 `Ok(values.len())` 不符：ON CONFLICT DO NOTHING 跳过的也算进去了 | 改注释为「返回取值条数」，或累计 rows_affected | safe |
| crates/semantic/src/ingest/autodiscover/register.rs:114 | `c.replace('\'',"")`/`n.replace('\'',"")` 两次 replace 两次全串分配 | 单次 `retain( | ch |
| crates/semantic/src/ingest/autodiscover/register.rs:114 | 只剥单引号不剥反斜杠：MySQL 默认反斜杠转义，字典值含 `\` 会吃掉闭合引号 → CASE 语法错，且该维度此后每次生成 SQL 必炸 | 一并剥 `\\`（补一条含反斜杠字典值的断言） | test |
| crates/semantic/src/ingest/autodiscover/register.rs:118 | `dim_code` take(80) 截断：两个长 （表，列） 前缀相同会被截成同一 dim_code，ON CONFLICT 互相覆盖，静默丢一个维度 | 截断时拼 8 位短哈希，或至少 warn 一次 | test |
| crates/semantic/src/ingest/autodiscover/register.rs:125-128 | `"{:.0}%"` 直接格式化 0.8~1.0 的 coverage：恒打印「抽样覆盖率 1%」（0.85→"1"）——每条 auto 维度的 description 都是错的，排障时严重误导 | 改为 `h.coverage * 100.0`（补断言：0.85→"85%"） | test |
| crates/semantic/src/ingest/autodiscover/register.rs:130-132 | DO UPDATE 刻意不动 status（人工 disabled 的 auto 维度重跑不复活）——这个刻意没有注释，后来者易顺手把 status 加进 SET | 加一行注释说明「不带 status 是刻意的」 | safe |

## principal.rs（10 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| principal.rs:44 | 错误文案「员工不存在」实际涵盖 disabled/deleted/密码过期三种情形，误导运维排障方向 | 文案改为「员工不存在或已停用/过期」 | test |
| principal.rs:61 | `c.trim() == rc` 只 trim 了 DB 侧，入参 `rc` 不 trim：三端传带空白的角色码会误报「该账号无角色」 | 比较前 `let rc = rc.trim();` | test |
| principal.rs:69-70 | 多角色错误文案 join 的角色名未过滤全空白项，可能出现「admin /  / city_manager」空段 | `.filter( | c |
| principal.rs:87-99 | `list_roles` 缺 `passwd_expire_time` 谓词，与 `load_principal`(L36-38) 口径不一：密码过期账号能列出角色却无法登录 | 补同样谓词 | test |
| principal.rs:99 | `list_roles` 结果不过滤 trim 后的空串，脏 role_code 会原样返给前端选择器 | map 后 `.filter( | c |
| principal.rs:66 | 合成 `(0, "admin".into())` 的 `role_id=0` 是魔数，靠「admin_shortcut 短路所以 0 永不进查询」这一隐式约定才安全 | 加行注释或 `const SYNTHETIC_ADMIN_ROLE_ID: i64 = 0` | safe |
| principal.rs:36-38 | SQL 字面量续行缩进不齐（`FROM` 行 9 空格、`AND` 行 11 空格），同文件其它 SQL 也不统一 | 对齐缩进 | safe |
| principal.rs:56 | `admin_flag.unwrap_or(0) == 1` 绕一层 | `admin_flag == Some(1)` | safe |
| principal.rs:64-72 | None 分支内嵌四层 match（Some/None × 1/0/n），主流程阅读要跳层 | 提为 `fn resolve_role(roles: &[(i64,String)], admin: bool) -> anyhow::Result<(i64,String)>` | safe |
| principal.rs:34 | 五元组类型注解无字段名注释，`Option<i8>` 对应哪列只能回数 SQL 列序 | 行尾注释标列名 | safe |

## web/vite.config.ts（9 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| web/vite.config.ts:7 | 无 `strictPort`:5180 被占用时 vite 静默改用 5181，开发者对着旧端口的页面调试旧进程，问题难复现 | 加 `strictPort: true` 让冲突直接报错 | safe |
| web/vite.config.ts:6-7 | `server` 无 `host` 也无注释：默认仅 localhost，局域网设备（手机/小程序联调机）访问不到 dev server，新人不知道为什么 | 加注释说明，或显式 `host: true` | safe |
| web/vite.config.ts:9 | proxy 目标 `127.0.0.1:8100` 无任何注释：这是 serve.ps1:91 起的 docker 容器后端；同一端口知识散落在 vite.config、nginx.conf:17、serve.ps1 三处，新人无从拼接 | 加一行注释指明「8100 = serve.ps1 起的容器后端」 | safe |
| web/vite.config.ts:9 | 后端地址写死，想指向其他环境（远程后端/裸机 exe）必须改源码 | 支持 `process.env.DMS_API_TARGET ?? 'http://127.0.0.1:8100'` | safe |
| web/vite.config.ts:9 | proxy 用字符串简写，无显式 `timeout`：后端长问数在 nginx 侧约定 300s(nginx.conf:23),dev 侧无任何超时约定，dev/prod 行为不一致 | 改对象形式并显式 `timeout: 300_000` 对齐 | test |
| web/vite.config.ts:4-11 | 无 `build` 段：生产无 sourcemap，线上报错堆栈是压缩后乱码，无法定位 | `build: { sourcemap: 'hidden' }`（生成但不引用，供排障上传） | safe |
| web/vite.config.ts:4-11 | 无 `manualChunks`:vue/pinia/ant-design-vue 全部打进 index chunk，每次发版 vendor hash 跟随变化，缓存全失效（echarts 已被 BiChart 异步 chunk 拆出，vendor 没有） | `build.rollupOptions.output.manualChunks` 把 vue 系/antd 拆成稳定 vendor chunk | safe |
| web/vite.config.ts:4-11 | 无 `define` 注入版本号（package.json version 0.1.0),UI 任何角落都看不到版本，用户报障只能靠猜 | `define: { __APP_VERSION__: JSON.stringify(process.env.npm_package_version) }`，关于页展示 | safe |
| web/vite.config.ts:4-11 | 无 `base` 注释：默认 `/`，若未来部署到子路径（如 dms-home 同域子目录）需改配置，现无任何提示 | 加注释说明「部署子路径时改 base」 | safe |

## scripts/docker-test.ps1（9 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| scripts/docker-test.ps1:35 | 硬编码 `RUSTUP_TOOLCHAIN=1.97.1-...`，L31 注释称「版本号与主机保持同一大版本」——但 rust-toolchain.toml:3 只写 `stable`，根本不钉版本号，注释与事实不符，容器版本漂移无人发现 | 注释改实话；或从 toolchain 文件派生版本 | safe |
| scripts/docker-test.ps1:55 | build 失败只看 `tail -20`，依赖编译失败时真正的首个 error 常在其上方（L52-54 注释只解决了退出码，没解决可见性） | 失败时提示「全量日志重跑：cargo build --locked」或加大 tail | safe |
| scripts/docker-test.ps1:63 | grep 摘要不含 `failures:` 名单行——失败测试的名字被过滤，只剩计数，要知道谁红必须重跑 | grep 加 `'^failures:'` 及其后续缩进行 | safe |
| scripts/docker-test.ps1:68 | 通过闸是 `fail=0 && targets>0`，未用 `pass`——全部测试被 ignore 时（0 passed 0 failed）也算绿 | 加 `[ "$pass" -gt 0 ]` | test |
| scripts/docker-test.ps1:69 | `.Replace('SEL', $Sel)` 字面替换：$Sel 含引号、`$`、`;` 等 bash 元字符直接拼进 bash -c，破句或注入（本地脚本风险低但属健壮性） | 校验 `$Sel` 只含 `[a-zA-Z0-9 \-=]` 再替换 | safe |
| scripts/docker-test.ps1:39-40 | volume 名 `dmsai_cargo`/`dmsai_target` 全局唯一硬编码，同一台机器第二个工作副本会共享 volume——cargo 锁竞争、产物互串 | volume 名带仓目录哈希后缀 | safe |
| scripts/docker-test.ps1:41 | 镜像 `rust:1-slim` 是漂移标签，与 L35 钉死 toolchain 的意图部分抵消（rustup/系统依赖随镜像版本变） | 钉 minor 标签（如 `rust:1-slim-bookworm`）并注释 | safe |
| scripts/docker-test.ps1:47 | Run-InDocker 失败即 `exit 1`，`-Only all` 时 build 红 test 直接不跑，且无一句「可用 -Only test 单跑」的提示 | [FAIL] 文案附用法提示 | safe |
| scripts/docker-test.ps1:24 | `$Sel` 传空串时 bash 侧 `cargo build --locked  ` 语义悄悄变 workspace 默认，与「用户传了什么就跑什么」的预期不符，无提示 | 空串时归一回 `'--workspace'` 并打一行说明 | safe |

## element.rs（9 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| element.rs:8 | 注释「search_text 变了需重跑 embed build 补向量」已过时——A9 自愈（embed_fill.rs:1-12）会自动补 NULL 向量 | 改注释指向 `MetaVecTarget::Element` | safe |
| element.rs:11-17 | `sync_elements` 四支非事务：中途失败留下混合态元素表（幂等可重跑，但窗口期召回读半成品） | 包事务或注释说明窗口可接受 | test |
| element.rs:20-108 | 四支逐行 `upsert_element`（元素数 × 1 次往返的 N+1） | `INSERT..SELECT` 或 `UNNEST` 批量 | test |
| element.rs:23,45,135-141 | 只 SELECT `status='active'` 且 upsert 从不写 `status`：指标/维度后被 disabled，其 element 行滞留 active 仍被向量召回（embed_fill.rs:59 按 active 选） | 同步置 disabled 或清孤儿行 | test |
| element.rs:128,131 | `push_str(&format!(..))` 临时分配 | `write!` 或分段 `push_str` | safe |
| element.rs:136-141 | `ON CONFLICT` 无条件 UPDATE 全列：内容没变也重写整行（写放大，每轮全量重写） | `ON CONFLICT .. WHERE` 判差异再更新 | test |
| element.rs:30,52,72,98 | 循环内 `&format!("metric:{code}")` 等每行临时 String | 复用可清空 buffer | safe |
| element.rs:22-26,44-48,66-70,90-94 | 四处 SELECT 均无 `ORDER BY`，失败重试时日志顺序不稳定 | 补 `ORDER BY` 主键 | safe |
| element.rs:11-17 | 四支顺序 `await`，同一 pool 可并发 | `try_join!` | test |

## lexicon.rs（9 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| lexicon.rs:21-27 | `ValueMap` 与 model.rs:267-278 `ValueRef` 重复类型 | 合并 | safe |
| lexicon.rs:100-102 | `longest_value_hit`：`sort_by_key` 的 key 函数 O(n log n) 次重算 `chars().count()`，filter 里还算过一次 | 预存 `(len, v)` 二元组再排 | safe |
| lexicon.rs:45-47 | `load_value_domains` 无 `ORDER BY` ⇒ `domain_rules`(caliber.rs:395) 产出规则序随库漂 | 补 `ORDER BY table_name, column_name` | test |
| lexicon.rs:72-77 | `load_domain_values` 无 `ORDER BY` | 同上 | test |
| lexicon.rs:125-128 | `load_value_maps` 无 `ORDER BY`，而 model.rs:288 同款查询却有——两处不一致 | 对齐 | test |
| lexicon.rs:14-18,21-27,32-37 | `TermDef`/`ValueMap`/`ValueDomain` 无 `Debug` | 补 derive | safe |
| lexicon.rs:92 | 注释引用 `recall_value_hints` 的 `>= 2` 门槛，阈值两处分仓维护 | 常量化或注释互指 | safe |
| lexicon.rs:105-109 | `load_terms` 无 asset-live 谓词（term 不挂表，设计如此）但无注释说明豁免原因，漂移守卫读费劲 | 补一句豁免注释 | safe |
| lexicon.rs:100 | `longest_value_hit` 对空 values 也走 collect+sort | 早退判空 | safe |

## datasource.rs（9 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| datasource.rs:106 | `upsert_datasource` 清 embedding 的 CASE 只比 `description`，而向量文本配方是 `name \ | \ | '。' \ |
| datasource.rs:180-184 | `register_upload_datasource` `ON CONFLICT` 改 description 却不清 embedding，与 106 行 upsert 行为不一致 | 同步清 embedding | test |
| datasource.rs:183 | 同上 `ON CONFLICT` 强制 `status='active'`：已停用的上传源重登记时被静默复活 | 确认是否有意；否则保留旧 status | test |
| datasource.rs:122-129 | `delete_datasource` 两条 DELETE 非事务：第二条失败留下 `kb.acl` 孤儿 → 重建同名 ds 时旧授权复活（正是 121 行注释防的事） | 包事务 | test |
| datasource.rs:83,91 | `&format!("SELECT {DS_COLS} ..")` 每次分配，`DS_COLS` 是 const | `concat!` 静态化 | safe |
| datasource.rs:26-34 | `DsPolicyConfig` 无 `deny_unknown_fields`：settings 里键名打错静默按缺省生效 | `#[serde(deny_unknown_fields)]` | test |
| datasource.rs:152-166 | `nearest_datasources` `k` 未夹紧，负 k PG 报错 | `k.max(0)` | safe |
| datasource.rs:149-151 | 注释只提 embed 缺席降级，未提醒结果未做可见性过滤（靠调用方与 `visible_datasources` 取交集），易被新调用方误用 | 注释补一句 | safe |
| datasource.rs:73-74 | 注释引用 `kb_api.rs:137`/`db.rs:95` 行号，行号注释易腐 | 改引符号名 | safe |

## dms_tables.rs（9 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| dms_tables.rs:280-293 | `fetch_str_in` 不过滤空白（对比 L308-314 `fetch_str_by_str_in` 过滤 trim 空白）：空串 login_name/actual_name/customer_code 会原样进 `ScopeSets` → `IN ('')` 垃圾条件 | 与 str 版对齐，末尾加 `.filter( | s |
| dms_tables.rs:46-52 | `guest_distributor_code` 在 `rows.len() > 1`（配置重复的数据事故）时静默返回 None → visitor 客户维度落哨兵拒绝，fail-closed 但无声 | `tracing::warn!("guest_distributor 配置 {} 行，拒绝", rows.len())` | safe |
| dms_tables.rs:51 | `rows.into_iter().next()` 在 `len == 1` 检查之后，绕 | `rows.pop()`（`mut rows`）更直白 | safe |
| dms_tables.rs:191 | `subordinate_ids` 结果 `HashSet→Vec` 顺序不定，下游 `login_names_by_ids`/`actual_names_by_ids` 的 IN 序与 trace/审计 diff 产生噪声 | 返回前 `sort_unstable()` | test |
| dms_tables.rs:169 | 注释「发现任一环边」不准：L184 检测的是「任一已访问节点」，菱形汇聚（两上级共一下属、非同层环）同样触发停钻 | 注释改为「发现任一已访问节点（含环边与菱形汇聚）」 | safe |
| dms_tables.rs:194,198,202 | `login_names_by_ids`/`actual_names_by_ids`/`customers_by_area_manager` 三个 pub fn 无 `///` 文档，同文件其它函数都有 | 补 doc（语义来源 Java 方法名） | safe |
| dms_tables.rs:218-229 | `common_customer_codes` 内联 fetch，返回不过滤空白（字典脏数据空 value_code 直入合并段） | 加 `.filter( | s |
| dms_tables.rs:151-164 | 双 `{in}` 模板的 bind 顺序靠注释钉，`for _ in 0..2` 的 2 是魔法数 | `const IN_SLOTS: usize = 2;` 并在注释点名 | safe |
| dms_tables.rs:8 | 头注释「逐行搬自 server/src/scope.rs:186-355」——该文件已删除，引用悬空 | 补「（原文件已删除）」 | safe |

## web/index.html（8 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| web/index.html:6 | 无 favicon：浏览器自动请求 `/favicon.ico`，被 nginx `try_files` 兜底返回 index.html(200、MIME text/html)，浪费一次请求且标签页无图标 | 加 `<link rel="icon" href="/favicon.svg">`（或 `href="data:,"` 显式空图标） | safe |
| web/index.html:6 | title 仅「DMS AI」纯英文缩写，与 `lang="zh-CN"` 的中文界面不一致，多标签页辨识度低 | 改为「DMS AI 问数」之类含中文语义的标题 | safe |
| web/index.html:5 | viewport 无 `viewport-fit=cover`,iPhone 刘海/灵动岛区域出现黑边 | `content="width=device-width, initial-scale=1.0, viewport-fit=cover"` | safe |
| web/index.html:9 | `#app` 完全为空：发版后旧 HTML 引用已删除的 hash 资产、或 main.ts 加载失败时，用户看到永久白屏且无任何提示 | 在 `#app` 内放兜底文案（如「加载失败请刷新」+重试链接）,mount 后自动被替换 | safe |
| web/index.html:9 | 无加载占位：1.1MB JS 下载/解析期间白屏无反馈，慢网体验差 | `#app` 内放轻量 loading 文本/CSS 动画 | safe |
| web/index.html:8-11 | 无 `<noscript>`:JS 被禁用时页面完全空白、无任何说明 | body 加 `<noscript>本应用需要启用 JavaScript</noscript>` | safe |
| web/index.html:3-7 | head 无 `<meta name="description">`，链接被分享/收藏时无摘要文本 | 补一句产品描述 meta | safe |
| web/index.html:3-7 | head 无 `<meta name="theme-color">`，移动端浏览器地址栏/状态栏为默认色，与品牌色脱节 | 加 `theme-color`（与 UI 主色一致） | safe |

## scripts/run.ps1（8 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| scripts/run.ps1:15 | 注释「元数据库端口只绑定本机回环」挂在 `docker ps` 检查行上方，与该语句无关（绑定事实在 docker/age/docker-compose.yml:10），位置误导读者以为这行脚本做了什么收窄 | 把注释挪到 compose 调用处（L25 前）或删掉 | safe |
| scripts/run.ps1:16 | docker daemon 未运行时 `docker ps 2>$null` 静默得 false → 走到 compose up 才报错，错误文案与根因（docker 没起）不符 | 先 `docker info`/`docker version` 探测，报「Docker 未运行」明文 | safe |
| scripts/run.ps1:18 | settings 文件 JSON 畸形时 `ConvertFrom-Json` 抛裸异常，无文件名上下文 | try/catch 后附「$settingsPath 不是合法 JSON」 | safe |
| scripts/run.ps1:19 | `pg_url` 键缺失或非法 URI 时 `[Uri]$settings.pg_url` 抛裸错，不提示是哪个字段 | 先判 `$settings.pg_url` 非空再转，throw 带字段名 | safe |
| scripts/run.ps1:8-14 | 允许回退 `settings.json`，但下游 serve.ps1:25 只读 `settings.docker.json`——回退路径走完整流程必在 serve.ps1 裸错，回退是假象 | 回退时打印警告「后续 serve 仍需 settings.docker.json」，或两脚本统一回退逻辑 | safe |
| scripts/run.ps1:20-22 | 密码做了 `UnescapeDataString`，用户名没有做——同一 UserInfo 两段处理不一致（含 `%40` 等编码的用户名会带错） | 用户名同样 Unescape | safe |
| scripts/run.ps1:25 | `\ | Select-Object -Last 2` 把 compose 输出截到 2 行，启动失败时丢失 compose 的真实报错 | 失败分支里提示 `docker logs dms-ai-pg`，或不截断错误流 |
| scripts/run.ps1:33 | `if ($LASTEXITCODE -ne 0)` 不可靠：ensure-services.ps1 全程无显式 `exit`，LASTEXITCODE 可能是调用前残留值；且 ensure-services 内部 throw 会经 ErrorActionPreference=Stop 直接终止本脚本，此检查半边是死代码。check-arch.ps1:141-142 记录过同形态「退出码不可靠」事故 | ensure-services 末尾显式 `exit 0`；run.ps1 改靠 try/catch 判成败 | test |

## crates/agent/src/answerers/entity/category.rs（8 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/agent/src/answerers/entity/category.rs:14 | `prefix_hint(cx.question)` 把问句**第三次**完整 parse（accept 一次、answer 一次、这里又一次），而调用方手里就有 parsed.kind | card 签名加 `explicit: bool` 由调用方透传 | safe |
| crates/agent/src/answerers/entity/category.rs:18,30 | LIKE 模式只 esc 单引号：`_` 通配符未转义（`%` 已被 parse_entity 拒，`_` 没有）——名称含 `_` 时模糊面悄悄变大 | 拼 pred 前把 `_` 转义为 `\_`（两库方言一致时），配测试 | test |
| crates/agent/src/answerers/entity/category.rs:46-50 | Rust 侧精确行判定 `value.trim() == name.trim()` 大小写敏感；MySQL `=`（ci  collation）却大小写不敏感——SQL 认为精确命中的行在这里认不出，落进候选分支 | 比较前 `to_lowercase`（或 eq_ignore_case 对 ASCII），配测试 | test |
| crates/agent/src/answerers/entity/category.rs:59 | `Candidate.code` 塞的是 `'class2'`/`'goods_category_name'` 字面量，候选卡「编码」列给用户看字段名——展示语义错位 | 编码列给空串或把层级挪到独立列，配测试 | test |
| crates/agent/src/answerers/entity/category.rs:68-69,80 | `selected[0]`、`r[0]` 直接下标索引；同文件其他取值都走 `.first()/.get()` 防御——风格与健壮性不一致（SQL 投影漂移即 panic） | 统一改 `.get(0).and_then(as_str).unwrap_or_default()` | safe |
| crates/agent/src/answerers/entity/category.rs:77-81 | `others` 不去重（同分类多行理论上去重过但 dim 脏数据可重复），且全角空格名会产出「试试：商品分类 」这类坏建议 | collect 后 `dedup`，且 map 里 trim 后非空才收 | test |
| crates/agent/src/answerers/entity/category.rs:95 | 商品清单查询失败 → `return Ok(None)` 整卡丢弃——分类名与商品数已在手，本可降级出无清单卡 | 失败时用空 RowSet 继续 build_card，配测试 | test |
| crates/agent/src/answerers/entity/category.rs:107 | 展示 SQL 只含 found_sql，实际还执行了 goods_sql——query_log/「查看 SQL」缺第二条（hits.rs:263 的做法是两条都留） | 拼接 `"; 商品清单\n{goods_sql}"`，配测试 | test |

## compound.rs（8 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| compound.rs:43 | `q.matches('和').count() + q.matches('与').count() >= 1` 两次全扫+计数只为存在性判断 | 改 `q.contains('和') \ | \ |
| compound.rs:131 | 拆解步 `llm.chat(req).await.ok()` 吞错零日志——拆解挂了静默退回单问，排障无痕迹（对比 87 行子问失败有 warn） | `match` 里 Err 分支补 `tracing::warn!(err=%e, "复合拆解 LLM 失败 → 不拆")` | safe |
| compound.rs:143-144 | `&r[s..=e]` 未防 `s > e`：LLM 回 `"]…["`（右括号先于左括号）时切片 start>end **直接 panic**，而 LLM 输出是不可信输入 | `if s <= e` 守卫后再切，否则回落 `vec![]` | test |
| compound.rs:145 | 剔空串用 `!s.trim().is_empty()` 判，但留存项不 trim——`"  各省销售额  "` 带空白进子问链路与汇总 prompt | `.map(str::trim)` 后再 filter | test |
| compound.rs:141-144 | `find('[')` 配 `rfind(']')`：数组前的散文含 `[`（模型照抄 system 里的示例格式）→ serde 解析失败 → 静默不拆，无任何日志 | 解析失败时 `tracing::debug!` 留一行；或从 `rfind('[')` 重试一段 | test |
| compound.rs:130 | 温度 `Some(0.1)` 字面量与 insight.rs:236、review.rs:38 三处重复，注释各自引一句「既定值」 | 抽 `pub(crate) const LLM_TEMP: f32 = 0.1` 共享 | safe |
| compound.rs:131-134 | `split_questions` 的 None 分支（LLM 失败/回垃圾 → 不拆 → try_compound None）无测试覆盖——现有 Fake 拆解步恒回两条 | 补一条「拆解步失败 → None 且不 panic」判据 | safe |
| compound.rs:87 | 子问失败 warn 只记 `sub` 与 `err`，不记「第几条/共几条」，批量失败时对不上总数 | 加 `idx`/`total` 字段 | safe |

## memory.rs（8 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| memory.rs:86 | 注释「按 30 天半衰衰减」与代码 `exp(-age/30)` 不符：半衰实为 20.8 天、30 天剩 1/e | 改注释为「30 天 1/e 衰减」 | safe |
| memory.rs:63-69 | SQL `LIMIT 10` 硬编码，`limit` 形参只在 rerank 后截断：传 `limit>10` 的调用者静默只得 10 条 | 注释钉住上限或 `LIMIT greatest(limit,10)` 绑定 | test |
| memory.rs:90 | `score` 对 `age_days<0`（时钟回拨/未来 `created_at`）给 exp 正增益 | `age_days.max(0.0)` | test |
| memory.rs:40-43 | `save_memory` `NOT EXISTS` 竞态：无唯一约束时并发产孪生行（与 exemplar 同款） | 确认唯一索引或 `ON CONFLICT` | test |
| memory.rs:102 | `bump_hits` 无 ds 谓词（id 来自 recall 本可信），豁免无注释 | 补一句豁免注释 | safe |
| memory.rs:63-70 | recall 无任何状态/生命周期过滤：`meta.memory` 无停用路径，旧经验只能物理删 | 观察项：加 enabled 列或定期清理 | test |
| memory.rs:66 | `1 - (embedding <=> $1)` 余弦相似度可为负/略>1（浮点），负 score 参与排序虽无害但无注释 | 注释说明或 clamp | safe |
| memory.rs:38 | `content` 截 400 但 `question` 不截——日志类字段都截（exemplar.rs:420），这里不一致 | `question` 同样截长 | safe |

## dialect.rs（8 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| dialect.rs:43,49 | 注释引用 `meta.rs:215-218/222-226` 硬行号，行号必漂 | 改为引用函数名/锚注释，去掉行号 | safe |
| dialect.rs:45-47,52-54 | MySQL 两条探针无 `ORDER BY`，schema 采集结果序不定（列探针明明查了 `ORDINAL_POSITION` 却不按它排） | 探针尾部补 `ORDER BY`（表名 / ORDINAL_POSITION） | test |
| dialect.rs:74-76,81-86 | PG 两条探针同样无 `ORDER BY`，同上 | 同上 | test |
| dialect.rs:45 | `TABLE_ROWS` 对未 ANALYZE/特殊引擎可为 NULL，`CAST(NULL AS CHAR)` 下游行解析易踩空 | 包一层 `IFNULL(...,'0')`（与 PG 侧 coalesce 对齐） | test |
| dialect.rs:76,85 | `relkind='r'` 不含分区表 `'p'`，PG 分区表会被探针整体漏采 | 改 `relkind IN ('r','p')` 或在注释里写明刻意排除 | test |
| dialect.rs:92 | `by_name` 每次 `to_ascii_lowercase()` 分配一个 String 只为查表 | 改用 `eq_ignore_ascii_case` 链式比较，零分配 | safe |
| dialect.rs:92 | 入参不 trim：`"mysql "`、`" pg"` 返回 None（fail-closed 但令人困惑） | `name.trim()` 后再匹配 | test |
| dialect.rs:99-118 | 测试只覆盖 `by_name`/`quote`；四条探针 SQL 从未验证过能被各自方言 parse | 加测试：`Parser::parse_sql(d.parser(), d.table_probe()/column_probe())` 必须 Ok | test |

## scripts/build.ps1（7 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| scripts/build.ps1:2 | WinLibs 路径硬编码且无 `Test-Path`，包不在时静默把死路径塞 PATH，回落到 Git 残缺 ld——正是注释要防的错，且无任何提示 | 加 `if (-not (Test-Path $mingw)) { throw '未找到 WinLibs…' }` | safe |
| scripts/build.ps1:5 | `Get-Process dms-ai-server` 只按进程名匹配，会杀掉别的目录/别的同名进程 | 按 `$_.Path` 过滤到本仓 target 目录再 Stop | safe |
| scripts/build.ps1:6 | 固定 `Start-Sleep 500ms` 不确认进程真退出，慢机器上 exe 句柄未放，os error 5 仍可能发生 | `Wait-Process -Timeout 5` 或循环确认进程消失 | safe |
| scripts/build.ps1:8 | 无 `$LASTEXITCODE` 检查：`cargo build \ | Select-Object -Last 15` 恒成功，编译失败脚本也 exit 0——docker-test.ps1:51-54 自己记录过同形态事故 | 末尾加 `if ($LASTEXITCODE -ne 0) { exit 1 }` |
| scripts/build.ps1:8 | 无 `--locked`，docker-test.ps1:55/62 均带 `--locked`，本机与容器可用不同 lockfile 解析结果 | 补 `--locked` | safe |
| scripts/build.ps1:8 | `Select-Object -Last 15` 截断输出，依赖编译失败时首个 error 常在最后 15 行之上，排障丢根因 | 失败时提示重跑不带截断的命令，或加大到 -Last 40 | safe |
| scripts/build.ps1:1-8 | 全脚本无 `$ErrorActionPreference = 'Stop'`，其余 5 个脚本都有，不一致 | 补上 | safe |

## docker/age/Dockerfile（6 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| docker/age/Dockerfile:2 | `FROM apache/age:latest`——PG 主版本随 latest 漂；跨主版本数据目录不兼容，某天 rebuild 后容器对着既有 named volume 直接起不来；docs/DEPLOY.md:9 写死「postgres16」已与漂移风险脱钩 | 钉主版本 tag（如 PG16 系）或 digest，并与 DEPLOY.md 对齐 | test |
| docker/age/Dockerfile:2 | 全文无一句解释为什么敢用 `:latest`——对照两个兄弟 Dockerfile 的理由密度，这里反常 | 加注释写明取舍（哪怕是「开发期接受漂移」） | safe |
| docker/age/Dockerfile:6 | `pg_config --version \ | grep -oE '[0-9]+' \ | head -1` 取首个数字：PG≥10 没问题，但基底若换成版本串带前缀数字的 fork 会静默得到错包名，报错点离根因远 |
| docker/age/Dockerfile:7 | pgvector 版本不钉（随 PGDG 浮动），向量索引行为（hnsw 参数/默认值）跨版本可能变 | 注释承认浮动，或钉 `postgresql-${PGMAJOR}-pgvector=<ver>` | safe |
| docker/age/Dockerfile:7 后 | 装完无自检：包名错了 apt 也会成功（装错包），要到 initdb `CREATE EXTENSION vector` 才炸 | 追加 `&& ls "$(pg_config --pkglibdir)/vector.so"` 构建期断言 | safe |
| docker/age/Dockerfile:全文 | 无 HEALTHCHECK（镜像自带 pg_isready） | 此处或 compose 加 `pg_isready -U postgres` | safe |

## gate.rs（6 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| gate.rs:121-123 | `check` 对一条 SQL parse 三次（is_safe_select_with / ensure_limit_with / table_names_of 各一次） | guard 侧透出已解析 AST 后这里复用（同 guard.rs:216 条） | test |
| gate.rs:6,10-11 | 注释两处写死「46 权限断言」，断言数随测试增减必漂 | 改成「既有字符串级断言套件」不写数字 | safe |
| gate.rs:56-59 | compile_fail doctest 硬编码 crate 名 `dms_kernel`，包改名即悄悄失效/报错 | 注释提醒「与 Cargo package 名耦合」 | safe |
| gate.rs:122 | 未加 LIMIT 时 `ensure_limit_with` 仍整串复制（guar227 的 Cow 化在这里直接受益） | 随 guard.rs:227 一并改 | safe |
| gate.rs:123 | `table_names_of(&text, d)?` 的 `?` 实际不可达（text 已在 121/122 两关 parse 成功）；一旦可达即说明前两关有洞，但今天无任何信号区分 | 加 `debug_assert!` 或注释说明该分支的性质 | safe |
| gate.rs:44-47 | `tables()` 返回的是 ast.rs:93 的「首段」语义（限定名=库名），doc 未提 | doc 补一句「限定名取首段」与 ast.rs:21 对齐 | safe |

## cache.rs（6 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| cache.rs:61-68 | `get` 两次 `m.get(k)` 哈希查找，未命中还多走一次 no-op 的 `m.remove(k)` | 单次 `match m.get(k)` 三分支重写 | safe |
| cache.rs:70-72 | `put` 无容量上限、无定期清扫：过期条目只在同 key 的 `get` 时惰性删除，多用户×多版本下 Map 内存只涨不缩 | put 计数到阈值（如每 64 次）做一次 `retain( | _,(_,at) |
| cache.rs:33,64,71 | 命中路径每请求 `scope.clone()` 整个 `Scope`（内含 6 个 `Vec<String>`），`put` 也 clone | Map 改存 `Arc<Scope>`，get 返回 `Arc<Scope>` | test |
| cache.rs:51-58 | 每请求 3 次 String 堆分配构造 Key，命中也照付 | `Key` 字段改 `Cow<'static, str>` 或提供 borrowed 查询键 | test |
| cache.rs:43-49 | `DefaultHasher::new()` 文档不保证跨 Rust 版本稳定；`ver` 仅需进程内一致这一点只隐含在注释里 | 注释补「不得把 ver 持久化/跨进程比较」 | safe |
| cache.rs:76-81 | `invalidate` 返回 0（login 大小写/拼写与管理面输入不符）时完全静默，运维以为清了 | 返回 0 时 `tracing::warn!` | safe |

## kernel/present.rs（5 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| kernel/present.rs:4 | 模块文档列举 `DIM_POOL`，实际常量名是 `DEFAULT_DIM_POOL`/`DWS_SALES_DIM_POOL`（semantic 侧），按名搜不到 | 文档改为真实常量名 | safe |
| kernel/present.rs:35,46-48,93-100,109-118 | `ColumnSpec`/`Block`/`Kpi`/`Delta`/`ViewSpec` 只 derive `Serialize`，没有 `Debug`——排障时 `{:?}` 打不出视图决策（Role/Semantic/ChartKind 反而都有 Debug，不对称） | 补 `Debug` derive（serde 形状零变化） | safe |
| kernel/present.rs:75 | `ChartKind` 无 `PartialEq`/`Debug`，semantic 测试只能 `matches!` 或序列化后断言（present.rs:546/556 的 panic 文案就是绕这个） | 补 `Debug, PartialEq, Eq` | safe |
| kernel/present.rs:52 | `Entity { pairs: Vec<(String, Value)> }` 用裸元组，serde 落 JSON 是 `[k,v]` 数组而非 `{k:v}`——已是线上契约不能动，但**没有任何注释说这件事**，后来人很容易「优化」成 map 破坏前端 | 在字段上加一行「元组序列化为二元数组，是前端契约」注释 | safe |
| kernel/present.rs:86 | `dir` 注释 `"up" \ | "down" \ | "flat"` 靠注释维持，semantic 侧写死字符串（present.rs:140），拼错无编译期防线 |

## crates/agent/src/answerers/knowledge.rs（5 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/agent/src/answerers/knowledge.rs:32 | 文档说「形参 6 个」，实际 7 个（weights 后加，35-36 行自己也在讲 weights）——注释与签名不符 | 改「形参 7 个」或去掉具体数字 | safe |
| crates/agent/src/answerers/knowledge.rs:79 | `split("#[cfg(test)]").next().expect(...)` 恒为 Some（split 至少产出一段），expect 是死代码且消息「文件头必然存在」文不对题 | 去掉 expect 直接 `.next().unwrap_or(src)` 或改消息为「split 首段恒存在」 | safe |
| crates/agent/src/answerers/knowledge.rs:80-81 | I5 断言只滤 `//` 行注释；若未来有人写 `/* Scoped … */` 块注释会误判红 | 判据注释里补一句「块注释不参与豁免」或同步滤块注释 | safe |
| crates/agent/src/answerers/knowledge.rs:84 | 针 `"Scoped"` 是裸子串，未来非测试代码里任何含 Scoped 的合法标识符/字符串都会撞红，误伤面没写进注释 | 在 needles 旁注释说明误判面，或改用更窄的 `"ScopedSql"` | safe |
| crates/agent/src/answerers/knowledge.rs:4-5 | 搬运源引用 `server/src/main.rs:569-576` 行号，源文件继续演化即失效 | 标注「（搬运时点行号）」 | safe |

## localize.rs（5 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| localize.rs:15 | `map_or(false, ...)` 是全仓唯一一处——ctx.rs:331、gather.rs:717、run.rs:149 等 12+ 处统一用 `is_some_and` | 改 `is_some_and(\ | s\ |
| localize.rs:35 | 去重键 `(col.clone(), code.clone())` 每条留痕两次堆分配，仅为插进 `seen` | 键改 `(col.as_str(), code.as_str())`（labels 存活期内借用合法） | safe |
| localize.rs:33 | `r.value_labels = ...` 无条件整体覆盖：今天 localize 是唯一写入点（grep 证实其余 13 处全是初始化 `vec![]`），但将来任何上游预填都会被静默吞掉 | `debug_assert!(r.value_labels.is_empty())`，或改 extend 后统一去重 | test |
| localize.rs:11-12 | 注释「空结果（反问/复合容器/0 列）直接过」把判据说成一个条件，实际 14-15 行是「主列与 supplemental 列**都**空」——单有 supplemental 列时会加载词表 | 注释补「两处都空」 | safe |
| localize.rs:28-29 | supplemental 的 redacted 用 `scratch` 接住即丢：若 `cn.apply` 对 supplemental 列标了脱敏，这条痕迹静默消失（27 行注释声明了字段缺失，但没说「丢痕迹」这一半） | 注释补半句；或 `debug!` 记 `scratch.len()` | safe |

## crates/semantic/src/ingest/mod.rs（5 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/semantic/src/ingest/mod.rs:27,30 | `ORIGIN_INFORMATION_SCHEMA`/`ORIGIN_UPLOAD` 全仓零写入方（grep 仅定义处与注释引用），ponytail 状态无任何判据钉着——列进 DDL 那天没人会被提醒回来加 bind | 加一条「column_doc 有 origin 列时必须用本常量」的牵引测试或 issue 跟踪 | safe |
| crates/semantic/src/ingest/mod.rs:42-47 | chars→map→filter→collect→replace×2→trim→take→collect 共 4 次 String 分配，热路径（每列注释都过）可一趟完成 | 单 pass 迭代器 + 按需 collect | safe |
| crates/semantic/src/ingest/mod.rs:47 | `replace("【⚠️","")` 留下孤儿 `】`（schema_sync.rs:180 测试把 `"台账忽略权限】"` 钉成预期），输出文案带一个无归属的右括号 | 连 `】` 一并剥或注释说明保留是有意，同步改测试 | test |
| crates/semantic/src/ingest/mod.rs:47 | trim() 在 take(120) 之前：截断点恰好落在空格前时结尾留尾空格（微小） | take 之后再 trim 一次 | safe |
| crates/semantic/src/ingest/mod.rs:54-66 | 测试只覆盖攻击样例与正常样例，缺边界：空串、纯空白、恰好 120 字、121 字 | 补边界断言（空进空出、120 不截、121 截到 120） | safe |

## proof.rs（5 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| proof.rs:38-41 | `argv.join(" ").starts_with(task)` 无 token 边界：argv 为 `meta autodiscover-x` 也能铸出 `meta autodiscover` 的凭证——凭证铸造面被前缀意外扩大 | 按空格 split task，与 `argv` 逐 token 做前缀相等比较 | test |
| proof.rs:38 | `std::env::args()` 遇非 UTF-8 参数直接 panic（Windows 环境变量/路径易触发），管理任务进程整体崩 | `args_os()` + `to_string_lossy()` | test |
| proof.rs:40 | `tracing::error!` 记录完整 `argv={argv:?}`，CLI 参数若含敏感值（DSN、临时密钥）会落日志 | 只记录前两个 token 或截断长度 | safe |
| proof.rs:34 | 注释引用「`main.rs:104`」已漂移：实际调用点在 `main.rs:301` 与 `main.rs:628` 两处 | 更新行号或改为不点行号 | safe |
| proof.rs:39 | `argv.join(" ")` 额外分配且元素含空格时 token 边界歧义（与 proof.rs:38-41 同根） | 逐 token 比较顺带消除 | safe |

## web/src/api.ts（4 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| web/src/api.ts:1 | 头注「后端端点的**单一事实源**」名不副实：全仓 ~90 处 fetch 直接内联 URL（App.vue/KbPanel.vue 等），本文件只有 1 个常量 | 头注改为「高漂移风险端点集中处」之类限定语 | safe |
| web/src/api.ts:10 | 自定规则「一次性的 fetch 不必进来」，但 ANALYSIS_URL 全仓仅 App.vue:2167 一处使用——恰好违反自己的收录规则 | 二选一：内联回 App.vue，或注释说明「收录理由是历史上两侧漂过」 | safe |
| web/src/api.ts:19 | JSDoc 列的请求体键缺 `deep`（实际体见 App.vue:2178），文档已落后于代码 | 补 `deep`（深度模式四段式开关） | safe |
| web/src/api.ts:21 | 同族端点 `/api/analysis/report`（App.vue:2208）就在 ANALYSIS_URL 隔壁使用却内联，同文件同族的漂移风险一碗水没端平 | 一并收录为 `ANALYSIS_REPORT_URL` 或在 api.ts 注释说明为何不收录 | safe |

## README.md（4 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| README.md:1 | 标题 `# dms_agent` 与仓库名 dms-ai、二进制 `dms-ai-server`（docker/server/Dockerfile:18）、工作目录名均不一致 | 统一为 dms-ai | safe |
| README.md:38 | 「创建本机 settings.docker.json」与 DEPLOY.md:20「裸机用 settings.json」、run.ps1:8-13（两者皆认、docker 优先）之间未说明取舍；本机启动节（:51-63）也没说配哪个文件 | 注明二选一及优先级 | safe |
| README.md:53 | 前置「PowerShell 7」无代码依据：scripts/*.ps1 均无 `#Requires -Version 7`，也未用 `??`/`?.`/三元等 7 独占语法 | 脚本加 `#Requires -Version 7`，或文档放宽为 5.1+（需实测） | test |
| README.md:83 | 「浏览器 E2E」无对应可执行物：web/package.json 无 e2e 脚本、tools/ 无 e2e 文件，仅 docs/PROGRESS.md:41 的历史 Playwright 手测记录 | 给出具体命令，或注明为手工步骤 | safe |

## crates/agent/src/answerers/mod.rs（4 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/agent/src/answerers/mod.rs:4 | 注释引「那 258 行」是拆分时时点快照，pipeline.rs 继续演化后数字必漂 | 标注「（拆分时点）」或去掉行数 | safe |
| crates/agent/src/answerers/mod.rs:33 | 「在 1.97 上不是 dyn 兼容的」钉了具体版本号，但 rust-toolchain.toml 是浮动 stable，版本论断无法复核 | 改引 lint 名 `async_fn_in_dyn_trait`，去掉版本号 | safe |
| crates/agent/src/answerers/mod.rs:49-50 | `accept` 的两行 doc comment 顶格（列 0），与 48 行缩进断裂，rustfmt 不会修 | 补 4 格缩进 | safe |
| crates/agent/src/answerers/mod.rs:89-102 | 「五个成员落地后」「不用等五个都到齐」两处「五个」已是历史数（现七成员），叙述易误导 | 改为「成员逐个落地的过程中」之类的无数量表述 | safe |

## builtin.rs（4 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| builtin.rs:146 | assert 文案错别字「manger」（应为 manager）；semantic 仓（seed_defs.rs:408）把 `manger` 列为禁用错拼，此处文案恰好复用了它 | 改为「不得用 manager 名称模拟稳定员工 ID」 | safe |
| builtin.rs:60-63 | 「对账单页面按 `manager` 裁决」注释说的是已退役的 `t_account_bill_header`（不在本文件），却夹在 `t_invoice_new_apply_header` 与 `t_device_inspection_header` 两条 insert 之间，归属易误读 | 注释开头点名「t_account_bill_header（已退役）」 | safe |
| builtin.rs:45 | `HashMap::new()` 无容量，39 次 insert 触发多次 rehash（低频路径，纯微优化） | `HashMap::with_capacity(39)` | safe |
| builtin.rs:66 | 无 owner 表清单用单行 for（L66）而 global 清单用多行 for（L97-107），两个同类清单排版不一 | 统一排版 | safe |

## lib.rs（3 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| lib.rs:10 | 注释「46 个权限单测（28 scope + 15 inject + 3 e2e）」已漂移：`tests/inject_tests.rs` 现有 21 个 `#[test]`，`fail_closed_tests.rs` 7 个完全未计入 | 改为不点数的描述或更新为实际计数 | safe |
| lib.rs:44-45 | kernel 错误被 `anyhow!("{e}")` 字符串化，丢失错误源链，且未附加「注入」上下文，排障时无法区分解析失败与规则缺失 | `.map_err(anyhow::Error::from).context("权限条件注入失败")` | test |
| lib.rs:15 | 预算注释「≤8 个 src + 4 个 tests」当前正好顶格（8+4），新人加文件无预警线 | 注释补一句「顶格中，新增先合并」 | safe |

## docker/age/init/01-extensions.sql（2 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| docker/age/init/01-extensions.sql:1-4 | 无「本脚本只在数据目录为空的首启执行一次」警示——改此文件对既有 volume 永不生效（官方 entrypoint 语义），新手改完看不到效果 | 头注加一行 | safe |
| docker/age/init/01-extensions.sql:1-4 | 扩展只建在默认库（POSTGRES_DB=dms_ai）；DEPLOY.md:36 让运维手工补，但 SQL 文件内无互链 | 头注指向 DEPLOY.md:36 那段 | safe |

## insight_api.rs 全文件（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| insight_api.rs 全文件 | 该文件是 CRLF/混合行尾，同目录 `corrector.rs`/`llm.rs` 是 LF——仓内行尾不一致 | 统一为 LF（或加 `.gitattributes` 钉死），一次性归一 | safe |

## vision_api.rs(main.rs（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| vision_api.rs(main.rs:1267 vs 1307) | 路由未抬 `DefaultBodyLimit`（kb upload 抬了），axum Json 默认 2MB：>2MB data:image 在抽取层被默认 413 拒，:88-90 的「图片大小不能超过 16MB」分支经此端点实际不可达 | 给 `/api/vision/chat` 加 DefaultBodyLimit（如 24MB），或收窄文档与校验阈值 | test |

## wework.rs：全文件（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| wework.rs：全文件 | 混合行尾（大量孤 `\r`，Read 已提示），diff 噪音与编辑器告警源 | 统一 LF | safe |

## crates/agent/src/answerers/mod.rs：全文件（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/agent/src/answerers/mod.rs：全文件 | 行尾 CRLF/LF 混用（Read 显示大量 `\r` 与无 `\r` 交错），diff 噪声与编辑器告警源 | 统一 LF（.gitattributes 或一次性 normalize） | safe |

## **pitfall.rs**（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| **pitfall.rs** |  |  |  |

## **mod.rs**（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| **mod.rs** |  |  |  |

## **ods.rs**（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| **ods.rs** |  |  |  |

## **schema.rs**（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| **schema.rs** |  |  |  |

## **metric.rs**（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| **metric.rs** |  |  |  |

## **cards.rs**（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| **cards.rs** |  |  |  |

## caliber.rs：全文件（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| caliber.rs：全文件 | 行尾混杂：主体 CRLF，1132-1136、1756-1774 等后加行是 LF（Read 已确认 lone-\r 提示），diff/编辑器噪音源 | 统一行尾（一次性 normalize，注意单独成 commit） | safe |

## crates/kernel/src/nl/time.rs 全文（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/kernel/src/nl/time.rs 全文 | CRLF/LF 行尾混合（lexicon.rs、qalog.rs 同病），diff 噪声源 | 统一 LF，配 .gitattributes | safe |

## crates/kernel/src/nl/lexicon.rs 全文（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/kernel/src/nl/lexicon.rs 全文 | CRLF/LF 行尾混合 | 统一 LF | safe |

## crates/kernel/src/nl/mod.rs（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/kernel/src/nl/mod.rs | 通读无问题（9 行，三行模块声明与文档一致） | — | — |

## crates/kernel/src/qalog.rs 全文（1 条）

| 位置 | 问题 | 修法 | 级别 |
|---|---|---|---|
| crates/kernel/src/qalog.rs 全文 | CRLF/LF 行尾混合 | 统一 LF | safe |

## DMS 后端源码校准项（专项调研，带源码证据）
- 订单额/订单数/成交客户数/客单价缺 order_type/is_points_order 过滤：SO04设备/SO10样品/SO12样品领用/SO15营销物料/SO16积分单全混入（Java SystemConsant.java:126-131 + salseOrder.xml:264-292 证据）→ 指标 scope_filter 补类型过滤（test，值会变，需业务确认+回归重签）
- t_sales_order.order_status 只有 3 档（暂存/无效/作废），Java 侧 17 档（SystemConsant.java:23-38）→ value_map 补齐 17 档；108 正名「已取消」、199「已删除」（safe）
- order_type 六值 value_map 缺失（SO01/SO04/SO10/SO12/SO15/SO16）→ 补登记（safe）
- 支付状态（0未支付/1已支付/2支付中）value_map 缺失 → 补登记（safe）
- 售后 after_sales_status 9 档 value_map 缺失（AfterSalesStatusEnum.java:19-30）→ 补登记（safe）
- 退款额含取消/驳回/退款失败单（Java 只认状态 4/5）→ 加 after_sales_status NOT IN ('6','7','9')（test，需业务确认）
- item_type 登记名「商品行」改「正品」对齐 Java 注释（safe）
- 权限缓存 15min TTL vs Java 即时失效（DataScopeManager 清 Redis+强制下线）→ 文档化或扩 scope_ver 源（test）
- 明细 join 键：Java 用双键（sales_order_code + sales_order_id），AI 单键；insight.rs:571 的 d.order_id 列名待核实 → 统一双键（test）
- 省区三套命名（DWS 存储值/运营归一/Java 字典）→ 术语表登记同义映射；川渝大区≠川渝藏（范围差异需业务确认）（safe）
- deleted_flag 个别表 bit default 1（database_ddl.sql:2134）→ 逐表核实例外登记（safe）
- 数量类聚合只取 item_type IN ('1','2')（结算行排除）→ value_map 注释补明（safe）

## 开源系统残余差距（调研）
- Y10 两级摘要运行时接线（能力层已全测备好，只差 chat 属主挂接——最大的一条鱼）
- SSE 流式推送（deep/progress 改 axum Sse，替轮询）
- LLM/embed 429/5xx 指数退避（公网抖动实测指纹）
- ask 崩溃恢复提示（沿 deep interrupted 收割先例）
- ClickHouse/ES 适配器（A6 计划内）
- MCP 客户端模式 / KB 部门级授权 / zip 入库 / RapidOCR 档 / Notion 连接器 / Trace DAG / Skills 自动打分 / 上下文快照表 / 动作预算 / admin 全局 dashboard

## 小程序集成点（调研）
- aiPendingQuestion storage 中转协议（所有带上下文入口的前置）
- 商品详情/搜索无结果/订单详情「问 AI」入口
- KB 答案 u-markdown 渲染（uni_modules 已有，零新依赖）
- citation 可点跳资料库、答案朗读（TTS hook 现成）、失败重试气泡、弱网本地队列
- 角色分层入口（hasMenuPermission）
