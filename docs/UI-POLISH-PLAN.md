# UI 美观 / 自学习账本 / 剩余准确项（2026-08-13 深设计）

> 4 路并行设计 + 1 路对抗验伪。WCAG 比值为设计方自行计算的实测值。


## visual-system

半套体系。theme.css 只有 37 行，颜色/圆角/阴影/字体四类有 token（`--primary`/`--text-*`/`--bg-*`/`--radius`×5/`--shadow`×3/`--ring`），间距、字号、层级三类**完全没有**，圆角虽有 token 却基本没人用：全仓 border-radius 字面量 68×6px + 27×8px + 26×5px + 24×999px + 16×7px，而 `var(--radius)` 只有 11 处（`--radius` 本身就是 6px）；字号 544 处声明分散成 26 档含 8.5/9.5/10.5/11.5px；gap 16 档（3/4/5/6/7/8/9/10/11/12/14/16/18/20px）；z-index 六个魔数（1000/1100/1140/1150/1200/1300）散在 6 个文件无排序说明。

暗色靠 `:root[data-theme="dark"]` 覆盖同名变量（theme.css:20-32），机制本身干净，但**入口是坏的**：App.vue:1105 默认 `'light'`，全仓零 `prefers-color-scheme`、零 `color-scheme`，且 applyTheme(:1718) 在 JS 挂载后才写 `data-theme` → 暗色用户每次刷新先闪一屏白。漏网硬编码 121 处：8 份遮罩 `rgba(17,24,39,.38)`（KbPanel 另有两份 `rgba(16,22,43,.42/.48)`，共 4 个不同 scrim 值）、23 处 `background: var(--primary); color: #fff`、ResultPanel.vue:910/912 与 KbAnswer.vue:605-621/667 两组语义色、四套并存的分类调色盘（BiChart 61-64、panel-utils.ts:41 GRAPH_PALETTE、DataMapPanel.vue:37/45——后者 7 色是 GRAPH_PALETTE 的严格子集）。

对比度实测（我按 WCAG 公式现算，脚本在 scratchpad）：`--text-faint #8d95ad` 对 bg-card 只有 **2.99:1**，对 bg-main 2.81、bg-sunken 2.55——连非文本的 3:1 都不过，却被 89 处小字复用；暗色 `#6b7390` 3.54:1 同样不过。暗色 `--primary #7b89f0` 上的 `#fff` 只有 **3.14:1**（23 处主按钮 + 用户气泡）。最严重的是 `--brand-ink` 暗色版 `#161c33→#2b3673` 贴在 `--bg-card #1a1e2b` 上做 `-webkit-text-fill-color: transparent` 的品牌字（App.vue:3405）：**1.01:1 到 1.49:1，暗色下侧栏 logo 是隐形的**。另外 OPTIMIZATION-PLAN W6#4 自己给的两个替换值都不成立：`#6f7791` 对 `--bg-main` 是 4.18 达不到它自己写的验收断言，暗色 `#9aa2bd`(6.55) 反而比 `--text-muted #8b93ad`(5.44) 更亮，三级层次被倒挂。

BiChart 配置整体专业（notMerge、null 断点不补零、TOP 排序 null 沉底、aria、双值轴、抽稀+旋转+hideOverlap、明暗双 token 缓存），三个真缺口：单色阶浅端 `#aeb6f2`/`#d1d6f8` 对白卡 1.95/1.43——>5 类走滚动图例时色块是唯一映射，等于不可读；TOP 收纳无任何告知（chartCaption:369-374 只说"各X贡献与排名"，200 行截 10 行用户不知情）；rows 变空时 render(:148) 直接 return 不 clear，上一张图留在屏上。移动端只有 6 个断点、`@media (max-width:820px)` 块里一条隐藏规则没有，触控热区全线 24-30px（`.hi-del/.hi-trace/.hi-clear` 三个裸 button 挤在 30px 行里且删除键紧邻）。嵌入 DMS 时 `embedded` 只做了隐藏"退出"一件事（App.vue:2695），268px 侧栏 + 品牌顶栏照常渲染 = 双层壳。

### ✅[AX119] 1. 暗色品牌渐变 1.01:1——侧栏 logo 在暗色下是隐形的（S，用户可见）

- 为什么：暗色用户打开侧栏看不到「🐯 皇家小虎」，只有一块空白；深度页 KPI 卡顶条（App.vue:3681 同样用 --brand-ink）在暗色下也整条消失。
- 文件：web/src/theme.css
- 改法：theme.css:23 暗色 `--brand-ink` 从 `linear-gradient(120deg,#161c33 0%,#2b3673 100%)` 改成 `linear-gradient(120deg,#aeb8ff 0%,#e8ebf6 100%)`（实测对 --bg-card #1a1e2b 分别 8.78:1 / 13.95:1）。亮色那行(:5)不动。
- 验收：web/tests 新增 theme-contrast.test.ts：解析 theme.css 提 --brand-ink 两个色标，断言暗色两端对 #1a1e2b 均 ≥4.5:1；人工在暗色下截侧栏头部。

### ✅[AX119] 2. --text-faint 2.99:1 不过 AA（并订正 W6#4 给错的两个值）（S，用户可见）

- 为什么：89 处小字用它：KPI 基期/变化额明细、深度表行数脚注、子任务验收断言、表格行号、图表加载态——恰好是判断数字可不可信要读的那批信息，办公室强光屏上读不出来。
- 文件：web/src/theme.css, web/tests/theme-contrast.test.ts
- 改法：theme.css:7 `--text-muted: #646d87 → #545c76`（6.63/6.23/6.07/5.65 对 card/main/body/sunken）、`--text-faint: #8d95ad → #67708c`（4.91/4.62/4.50/4.19）。theme.css:25 暗色 `--text-muted: #8b93ad → #a6aec7`（7.51）、`--text-faint: #6b7390 → #8a92ad`（5.37）。**不要用 W6#4 写的 #6f7791**（对 --bg-main 只有 4.18，过不了它自己的验收断言）与 **#9aa2bd**（6.55 比 muted 5.44 更亮，三级层次倒挂）。
- 验收：theme-contrast.test.ts 用 node:test 读 theme.css 现算 WCAG：断言 --text-faint 对 --bg-card/--bg-main 均 ≥4.5、且 muted 的比值严格大于 faint（钉住层次不倒挂），明暗各一组。

### ✅[AX131] 3. 暗色主色按钮上的 #fff 只有 3.14:1（23 处）（S，用户可见）

- 为什么：暗色下每一个主按钮（发送、上传/管理、生成周报、追问选项、模式切换选中态）和用户自己的提问气泡文字都不过 AA。
- 文件：web/src/theme.css, web/src/App.vue, web/src/DataMapPanel.vue, web/src/KbAnswer.vue, web/src/KbDocPreview.vue, web/src/KbEval.vue, web/src/KbGraph.vue, web/src/KbMindmap.vue, web/src/KbPanel.vue, web/src/ResultPanel.vue, web/src/SkillsPanel.vue
- 改法：theme.css:3 加 `--on-primary: #fff`，:21 加 `--on-primary: #11141d`（对 #7b89f0 = 5.85:1）。把 23 处 `color: #fff`（App.vue:3410/3455/3474/3552/3634/3657/3746/3769/3849/3875、DataMapPanel:1032/1033、KbAnswer:655/662、KbDocPreview:772、KbEval:511、KbGraph:1084、KbMindmap:861、KbPanel:3349/3354、ResultPanel:1115、SkillsPanel:285/286）改 `var(--on-primary)`。KbPanel:3354 的 .danger-btn 底色是 --error-text，单独留 #fff（暗色 #ec8f8f 上黑字更好，可同刀加 --on-error）。
- 验收：源码断言：全仓 `background: var(--primary)` 的规则里不再出现 `color: #fff`；暗色下逐个截图主按钮与用户气泡。

### ✅[AX131] 4. BiChart 单色阶浅端 1.43:1——饼图图例色块在白卡上看不见（S，用户可见）

- 为什么：6 类以上走滚动图例、不画扇区标签（BiChart.vue:167-168），色块是名字与扇区之间唯一的映射；最浅两阶 #aeb6f2/#d1d6f8 对白卡 1.95/1.43，用户看到的是「有名字、找不到对应扇区」。
- 文件：web/src/BiChart.vue
- 改法：BiChart.vue:63-64 两条单色阶换成等对比步进版：`LIGHT_MONO = ['#2b3aa6','#3e4cae','#505db6','#626ebd','#747dc4','#8790cd']`（对白卡 9.30/7.37/5.86/4.68/3.84/3.04，全部过 3:1）、`DARK_MONO = ['#6776ee','#818ef1','#98a3f4','#aeb7f6','#c3c9f8','#d6dafb']`（对 #1a1e2b 4.27→12.08）。
- 验收：给 web/tests 加一条同 theme-contrast 的算式断言：BiChart.vue 里两条 MONO 数组每个色值对 #ffffff / #1a1e2b ≥3:1；本地对一个 6 类饼图明暗各截一张。

### ✅[AX119] 5. TOP 收纳静默截断：200 行只画 10 根柱，标题一个字不说（S，用户可见）

- 为什么：「今年各客户销售额」有 200 个客户时图上只有 10 根，caption 写「各客户贡献与排名」，用户会把这 10 个当成全部——这是把「图对不对」赌在用户自己去数表格行数上。
- 文件：web/src/ResultPanel.vue
- 改法：ResultPanel.vue:369 `chartCaption(block, view)` 加第三参 `rows = props.result.rows`，函数末尾返回前拼后缀：`const n = block.top; return n && rows.length > n ? `${base}（前 ${n} 项，共 ${rows.length} 项）` : base`。补充区两处调用（:723 与 :814/:827 对应的 supplemental 分支）传 `supplemental.rows`。BiChart 不动。
- 验收：把 chartCaption 一起搬进 W6#12 计划里的 result-view.ts，加 node:test：top=10/rows=200 → 含「共 200 项」；top=null 或 rows≤top → 与今天逐字相同。

### ✅[AX119] 6. rows 变空时 BiChart 留着上一张图不清（S，用户可见）

- 为什么：追问把结果打成 0 行（权限收窄、时间窗改空）时，图表区显示的还是上一轮的柱子和上一轮的坐标轴——用户看到一张与当前数字无关的图，且没有任何提示。
- 文件：web/src/BiChart.vue
- 改法：BiChart.vue:148 `if (!props.rows.length || !props.y.length) return` 改成 `if (!props.rows.length || !props.y.length) { chart.clear(); return }`。空态文案交给调用侧：ResultPanel.vue:698/719 两个 `<article class="chart-card">` 的 v-for 外层 section 已有 v-if，再给 BiChart 同级补 `<p v-if="!result.rows.length" class="chart-state">本轮无数据可绘</p>`（.chart-state 样式 :1003 现成）。
- 验收：node:test 覆盖不到 echarts，用源码断言 `chart.clear()` 存在即可；人工：先问一题有数据的，再追问一个必然 0 行的（如不存在的门店），确认图表区变成文案而不是残留旧图。

### ✅[AX119] 7. 暗色首屏闪白 + 不认系统偏好 + 原生控件不跟主题（S，用户可见）

- 为什么：暗色用户每次刷新先看到一整屏白再翻黑；系统设成暗色的新用户第一次进来是亮色；暗色下 <select> 下拉面板、滚动条槽、日期控件仍是亮色原生样式（KbPanel 的上传目标/空间选择器、设置页 .vf-select 都是 select）。
- 文件：web/index.html, web/src/theme.css, web/src/App.vue
- 改法：三处：①index.html <head> 末尾加 4 行内联 script（零依赖）：`<script>{const t=localStorage.getItem('theme')||(matchMedia('(prefers-color-scheme:dark)').matches?'dark':'light');document.documentElement.setAttribute('data-theme',t)}</script>`；②theme.css:2 的 :root 加 `color-scheme: light`，:20 的暗色块加 `color-scheme: dark`；③App.vue:1105 默认值改成 `localStorage.getItem('theme') || (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light')`，与内联脚本同一表达式。
- 验收：源码断言 index.html 含 data-theme 内联脚本、theme.css 两处 color-scheme；人工：系统设暗色 + 清 localStorage 刷新，确认无白闪且直接是暗色，打开一个 <select> 看下拉是暗色。

### 8. 遮罩层 4 个硬编码值 + 暗色模态零分离（W6#10 的暗色缺口）（M，用户可见）

- 为什么：暗色下 rgba(17,24,39,.38) 盖在 #11141d 的页面上等于什么都没做，而 --bg-card #1a1e2b 与 --bg-body #11141d 只差 1.11:1——弹窗和背景糊成一片，用户看不出哪层是模态。
- 文件：web/src/theme.css, web/src/App.vue, web/src/DataMapPanel.vue, web/src/SkillsPanel.vue, web/src/SqlAuditPanel.vue, web/src/UsagePanel.vue, web/src/KbPanel.vue
- 改法：theme.css:2 加 `--scrim: rgba(17,24,39,.38); --bg-elevated: #ffffff;`，:20 加 `--scrim: rgba(0,0,0,.62); --bg-elevated: #242a3a;`（对被压暗后的背景 1.37:1，再配 --border 与 shadow-lg 足够分层）。theme.css 补一组共享类 `.ui-mask{position:fixed;inset:0;background:var(--scrim);backdrop-filter:blur(5px)}` 与 `.ui-dialog{background:var(--bg-elevated);border:1px solid var(--border);border-radius:var(--radius-md);box-shadow:var(--shadow-lg)}` + 一个 `@keyframes uiSpin`。替换 10 处遮罩（App.vue:3592/3750/3864/3921、DataMapPanel:1014、SkillsPanel:257、SqlAuditPanel:273、UsagePanel:174、KbPanel:2975/3357——后两处今天用的是另两个值 rgba(16,22,43,.48/.42)），弹窗底盘改 `class="ui-dialog xx-dialog"`，删掉 dnSpin/dmSpin/skSpin/saSpin/upSpin 五个同义 keyframes。顺手给 SkillsPanel/SqlAuditPanel/DataMapPanel 的 <style> 补 scoped。
- 验收：源码断言全仓 `@keyframes .*Spin` 只剩 1 个、`rgba(17, 24, 39` 与 `rgba(16, 22, 43` 各 0 次；六个面板明暗双主题逐个截图，暗色下确认弹窗边界清晰。

### 9. 触控热区全线 24-30px，删除键紧贴查看键（S，用户可见）

- 为什么：手机上点会话列表里的「🗑」经常点成「🔍」或反过来，误删会话不可撤销；顶栏 8 个 26px 高的 btn-sm 在 375px 宽下摊 3-4 行，本来就窄的屏再被 chrome 吃掉。
- 文件：web/src/App.vue
- 改法：App.vue:3917 的 `@media (max-width: 820px)` 块内加三条（今天这个块里一条尺寸规则都没有）：①`.btn-sm, .btn-icon, .btn-mini, .pill, .ask-opt { min-height: 44px }` 并给 .btn-sm 补 `padding-inline: 14px`；②`.hist-item .hi-del, .hist-item .hi-trace, .hist-item .hi-clear { width: 44px; height: 44px; display: inline-grid; place-items: center; opacity: 1 }`（同刀让它们在触屏常显，与 :3534 的 @media(hover:none) 一致）；③`.topbar .btn-sm:not(.mobile-kb):not(.mobile-weekly) { display: none }`，把使用统计/提示词包/数据地图/SQL审计/设置以 `.sec` 形态补进侧栏抽屉。**别新造移动端菜单组件**。
- 验收：result-layout.test.ts 同风格源码断言：≤820px 块内含 min-height: 44px 与 topbar 隐藏规则；375×667 实机点 10 次删除/查看确认不误触，抽屉里入口齐全。

### 10. 嵌入 DMS 首页双层壳：268px 侧栏 + 自有品牌顶栏照常渲染（S，用户可见）

- 为什么：integrations/dms-home/index.vue:124 已经用 calc(100vh - 112px) 给 DMS 自己的顶栏留了位，里面又渲染一遍侧栏（含「🐯 皇家小虎」）与顶栏（含「数据智能 · DMS 自然语言取数」）——两层导航两个品牌，内容区白丢 268px 横向空间。
- 文件：web/src/App.vue
- 改法：App.vue:2587 `<div class="wrap" :class="{ 'has-preview': !!preview }">` 改成 `:class="{ 'has-preview': !!preview, embedded }"`（embedded 已是 :1085 的 ref）。样式块加两行，直接复用 :3918-3919 已有的抽屉规则：`.wrap.embedded .side { position: fixed; top:0; left:0; bottom:0; z-index:1150; width:min(300px,86vw); transform: translateX(-105%); transition: transform .18s ease-out }` 与 `.wrap.embedded .mobile-menu { display: inline-flex }`。品牌区先不动（是口味不是缺陷，找产品确认一次）。
- 验收：按 integrations/dms-home/README.md 起 DMS 前端 + Agent 打开首页，截图确认只有一层左侧导航、内容区宽度增加约 268px、☰ 能拉出会话列表与主题切换。

### 11. metric-card 两份真相源在互相打架（M，用户可见）

- 为什么：ResultPanel.vue:951 里那两个 `text-transform: none; letter-spacing: 0` 存在的唯一理由就是抵消 App.vue:3682 的 uppercase + .05em——App.vue 的 <style> 没 scoped，两份 .metric-card/.mc-* 同时命中同一批元素。谁改 App.vue 那半边，结果卡的 KPI 就悄悄换样。
- 文件：web/src/App.vue, web/src/ResultPanel.vue
- 改法：删掉 App.vue:3680-3687 的 .metric-card/.mc-label/.mc-val/.mc-delta/.mc-vs 六条（深度页那份），深度页 KPI 改用 ResultPanel 的形态；同刀删 .dkpi/.dk-*(:3799-3813)、.dh-card(:3815-3818)、.df-card(:3796-3798) 三套同义卡片样式，模板换成 .metric-card + .sc-cell。ResultPanel.vue:951 的 `text-transform: none; letter-spacing: 0` 两个抵消声明随之删掉。约净删 30 行样式。
- 验收：取一条真实深度报告，改造前后逐 section 截图比对 KPI 卡的标签/数值/环比三行；源码断言 App.vue 中 `.mc-label` 与 `.dkpi` 出现 0 次。

### 12. 表单控件边框 1.25:1，不过 WCAG 1.4.11（M，用户可见）

- 为什么：输入框/下拉/描边按钮的边界靠 `1px solid var(--border)`，--border #e2e6ef 对白卡只有 1.25:1（暗色 #2c3247 对 #1a1e2b 1.31:1）——「这里能输入」这件事本身看不出来，只有点进去才知道。
- 文件：web/src/theme.css, web/src/App.vue, web/src/KbPanel.vue, web/src/DataMapPanel.vue, web/src/ResultPanel.vue
- 改法：theme.css:8 加 `--border-strong: #888fa6`（对 card/main 3.22/3.02），:26 加 `--border-strong: #636b8c`（对 dark card 3.17）。只把**表单控件**的 border 换过去，不动卡片/分割线：App.vue .f-item input/select、.vf-select(:3436)、.inputbar textarea(:3745)、.weekly-field input(:3761)、.login-box input(:3787)、.steer-input(:3733)；ResultPanel .ask-input(:1113)；KbPanel :3006/3068/3137/3191/3241/3365；DataMapPanel .dm-path input(:1025)/.dm-search(:1045)。
- 验收：theme-contrast.test.ts 加断言 --border-strong 对 --bg-card 明暗均 ≥3:1；源码断言上述选择器不再出现 `1px solid var(--border)`；截一张设置页与输入区。

### 13. 圆角六档并存（token 已就位，只是没人用）（M）

- 为什么：同一屏里 .foundation/.metric-card/.chart-card 是 8px、.dkpi/.insight-card/.sql-details 是 7px、.dtable-wrap/.dsec-seg 是 5px、大量按钮 6px——相邻卡片圆角不一致是「这套 UI 没人统一过」最直观的信号。
- 文件：web/src/App.vue, web/src/ResultPanel.vue, web/src/KbPanel.vue, web/src/KbAnswer.vue, web/src/KbDocPreview.vue, web/src/DataMapPanel.vue, web/src/SkillsPanel.vue, web/src/SqlAuditPanel.vue, web/src/UsagePanel.vue, web/src/KbEval.vue, web/src/KbGraph.vue, web/src/KbMindmap.vue
- 改法：收成三档，全部用**已有**的 token（不新增变量）：5px/6px/7px → `var(--radius)`(6px)，8px/9px/10px → `var(--radius-md)`(8px)，12px → `var(--radius-lg)`，99px/999px/50%（非圆形元素）→ `var(--radius-full)`。sed 可做，但按文件分批提交，`border-radius: 50%` 只在真圆点/头像上保留（.insight-dot、.trust-badge i、ol li::before、.spin）。共约 140 处。
- 验收：源码断言全仓 `border-radius: [0-9]` 只剩 50% 与 0 两类；改完逐面板截图对拍（预期只有 5px→6px、7px→6px、9/10px→8px 的亚像素差）。

### 14. NODE_PALETTE 是 GRAPH_PALETTE 的严格子集，重写了一遍（S）

- 为什么：数据地图与知识图谱是同一产品里两张力导向图，今天节点配色来自两份各自维护的数组（7 色全部已在 10 色里），改一边另一边不动——同屏视觉语言漂移的经典配方，而 panel-utils.ts:40 的注释本来就写着「共用一份，同屏视觉语言不漂移」。
- 文件：web/src/DataMapPanel.vue, web/src/panel-utils.ts
- 改法：删掉 DataMapPanel.vue:45 的 `NODE_PALETTE`，改 `import { GRAPH_PALETTE } from './panel-utils'` 并在取色处用 `GRAPH_PALETTE[i % GRAPH_PALETTE.length]`；:47 与 :86 的 `'#8b93ad'` 兜底改成读 `--text-muted`（DataMapPanel 已有 dark 判定，KbMindmap.vue:623-627 的 themeColor() 是现成写法，搬过去用）。净删一个常量。
- 验收：源码断言 DataMapPanel.vue 不再出现 `NODE_PALETTE` 与 `#8b93ad`；明暗两主题各打开一次数据地图与知识图谱，确认同类型节点同色。


## result-presentation

**一屏之内先看到什么。** 桌面 1440 宽、侧栏 268 的实测顺序是：`.res-meta` 操作行（App.vue:2994 起，行数 + 最多 6 个文字按钮）→ 收起的 `<details class="foundation">`（ResultPanel.vue:584-600，min-height 42 + margin 14）→「AI 结论与建议」卡片区（:643）→ 才是 `.kpi-section`（:661）。也就是说数字排在第 4 位，前面 250-430px 是操作条、审计条和一段散文。`.foundation` 只在 `trust=review` 或 `coverage=blocked` 时展开（:587），正常路径下它既占了首屏最贵的一格、内容又一个字看不见——这是最典型的「既喧宾夺主又被忽略」。而同一产品的知识侧相反：KbAnswer.vue:482 的 `.answer-receipt` 恒常显，把「本轮实际按…检索」直接摆在正文之前。两半对同一类信息的处置完全相反。

**收据本身的层级是对的，位置错了。** `understandingText`（ResultPanel.vue:189-191）是「智能」这根轴唯一的用户可见证据，今天在 `.foundation-body` 里（:602-604），追问「那上个月呢」必须点开折叠条才能确认系统解成了什么。trust 徽标（:590-595）留在 summary 是合理的降噪结果（PROGRESS.md:1892 的裁决），但徽标旁那句静态标题「问题理解与结果依据」是零信息量的，把 understandingText 换上去就等于零新增高度换一条常显证据。

**「AI」这个 kicker 贴错了。** 走到 `insightCards` 的 insight 全部是确定性产物：present.rs:172「确定性 0-LLM」的排行/趋势算术、business_lookup.rs:195/215/641 与 entity.rs:956 的 `format!` 模板；compound.rs:101 / hybrid.rs:148 那两份 LLM 文本走的是 subs 分支并被 `dataOnlyResult`（App.vue:2363）剥掉。于是 entity.rs:956 的「已按最小权限原则拒绝展示」这条权限裁决，正顶着 ResultPanel.vue:646 的「AI」角标当「结论与建议」渲染——用户被训练成对 AI 文本打折，最可信的一行反被贴上最不可信的标签。

**数字格式化基本统一，两处漏了。** format.ts 是唯一真源，`isGrossMarginLabel`/`semanticForLabel`/`compress` 三处渲染器（ResultPanel.displayValue、BiChart.metricNumber、App.formatMetricValue）都接同一份。漏的是深度页环比：deep_api.rs:339 的 `pct` 恒按相对百分比算，App.vue:2273 `comparisonRate` 恒输出 `%`；而 present.rs:133-142 对 Percent 指标出的是**百分点**，ResultPanel.vue:352-356 也照百分点渲。结果是同一张深度 KPI 卡里，`comparisonRate` 出「+1.7%」、紧挨着的 `signedComparison`（App.vue:2285-2292）出「+0.33 个百分点」，自己打自己。

**长表/长文档/空态/错误态/流式。** 表格有两套语言：问数页 `.tbl-wrap`（ResultPanel.vue:1015-1035）max-width 320 省略号 + sticky 行号 + 渲染全部行，深度页 `.dtable`（App.vue:3852-3862）nowrap + min-width 680 + 无行号 + 客户端截 24 行（App.vue:2261）——后者正是 ResultPanel.vue:497-513 那段长注释明令废除的「前端持有行数上限」。空态与错误态没问题（`.empty-hint` warning 档、`.caliber-warn` error 档、`.bubble.err` 带重试/续跑/角色选择，分级清楚）。流式两处掉链子：delta 分支（App.vue:1536-1543）不跟随滚动（`scrollDown` 只在 1642/1685/1971/2031/2124 五个提问/切会话点调），以及 KbAnswer 每个 delta 重跑 `presentation()`（:302-348），标题与「直接结论」框会随半截 markdown 抖动。

**移动端。** 375×667 实算：顶栏 8 个 `btn-sm` 摊 3-4 行 ≈150px，`.quick` 摊 2-3 行 ≈100px，输入区 66px，常驻 chrome 约 316px（47% 屏高）；同时 `.chat` 24px（App.vue:3549）与 `.bubble` 16px（:3551）的内边距在任何断点都不缩，结果表实际可用宽只有 293px，其中 38px 还被 sticky 行号占掉。`.res-meta`（:3577）无 `flex-wrap`，6 个按钮被压成锯齿。

**可访问性。** `--text-faint` #8d95ad 对白实测 2.99:1（相对亮度 0.3017），全仓 91 处引用，落点恰是 KPI 基期/变化额、深度表行数脚注、板块验收断言——判断数字可信度要读的那批。这是明令不许省的一类。

### 1. 移动端 chrome 收口：工具入口进抽屉、快捷条横滑、正文内边距减半（M，用户可见）

- 为什么：375×667 上顶栏+快捷条+输入区常驻约 316px（47% 屏高），答案只剩一半；结果表横向可用宽仅 293px（375−48 chat −32 bubble −2 边框），38px 还被 sticky 行号吃掉。任何一次移动端提问都能一眼看出变化。
- 文件：D:\code\dms_ai\web\src\App.vue
- 改法：①给 App.vue:2689-2694 的六个工具按钮（使用统计/提示词包/数据地图/SQL审计/设置，以及 2688 的知识库保留）统一加 class `tool`：`class="btn-sm tool"`；②抽屉里照抄 2599-2601「知识库」那个 `.sec` 的markup 加一节：`<div class="sec" v-if="sessionToken"><div class="sec-t">工具</div><button class="btn-sm" @click="usageOpen=true; sideOpen=false">📈 使用统计</button>…</div>`（5-6 个按钮，退出钮同节末尾）；③`@media (max-width:820px)` 块（:3917）内加四条：`.topbar .tool{display:none}` / `.topbar .dms-user{display:none}` / `.quick{flex-wrap:nowrap;overflow-x:auto;scrollbar-width:none}` + `.quick::-webkit-scrollbar{display:none}` / `.res-meta{flex-wrap:wrap;row-gap:6px}` / `.chat{padding:14px 12px}` / `.bubble{padding:10px 12px}`。不新造任何「移动端菜单」组件。
- 验收：web/tests/result-layout.test.ts 加同风格源码断言：820px 媒体块内含 `.topbar .tool{display:none}`、`.quick` 的 nowrap、`.chat{padding:14px 12px}`；375×667 实机确认抽屉里六个入口都在、快捷条可横滑、`.res-meta` 两行不出锯齿、结果表可用宽由 293→329px。

### 2. 「本轮理解」提进 foundation summary（零新增高度换一条常显证据）（S，用户可见）

- 为什么：追问「那上个月呢」时，系统把问句解成了什么是唯一能自证「懂了」的证据，今天埋在默认收起的 details 里（ResultPanel.vue:587 只在 review/blocked 才 open），而 summary 那格占着首屏却只放了一句零信息量的静态标题。知识侧 KbAnswer.vue:482 本来就常显同类信息——同一产品两半相反。
- 文件：D:\code\dms_ai\web\src\ResultPanel.vue
- 改法：ResultPanel.vue:596 改成 `<span class="foundation-title" :title="understandingText || undefined">{{ understandingText || (intentSummary ? '问题理解与结果依据' : '结果依据') }}</span>`；删掉 602-605 四行（避免同句出现两次）；样式 :899 `.foundation-title` 补 `min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;`，并在 :1147 的 `@media(max-width:600px)` 块里加 `.foundation-title{white-space:normal}`。hasFoundation(:192) 与 :open 条件(:587) 一字不动。
- 验收：源码断言 `understandingText` 出现在 `<summary>` 与 `</summary>` 之间且不再出现在 `foundation-body` 内；回归：先问「本月销售额按省区」再追问「那上个月呢」，第二轮不点开任何折叠条就能读到「…2026-07…」。

### ✅[AX123] 3. 混合结果的「AI 综合分析」被渲染两遍（S，用户可见）

- 为什么：一道混合问（数据+资料）会同时命中 App.vue:3183（`t.result?.kb && t.result.view?.insight`）与 3193（`t.result.subs?.length && compoundAnalysis(...)`），而 compoundAnalysis(2381-2384) 返回的正是 `userFacingMarkdown(result.view.insight)`——同一段文字、同一个「AI 综合分析」标题，上下紧贴出两块。
- 文件：D:\code\dms_ai\web\src\App.vue
- 改法：App.vue:3189 的 `<div v-if="t.page?.insight" class="ai-panel deep-insight">` 改成 `v-else-if`，把 3183/3189/3193 三块并成一条 v-if 链（深度页恒有 t.page、混合恒无，两者互斥，链接安全；中间只隔注释，Vue 编译器跳过注释节点匹配 v-else-if）。
- 验收：result-layout.test.ts 加一条断言：`t.page?.insight` 那行以 `v-else-if` 开头；实测一道混合问（「烤肠退货政策，以及本月烤肠销售额」）截图确认只剩一块 AI 综合分析。

### ✅[AX123] 4. 流式回答不跟随滚动：10-20 秒的生成用户看到的是静止画面（S，用户可见）

- 为什么：App.vue:1536-1543 的 delta 分支只改 `aiTurn.result`，一次都不滚；`scrollDown`(1974-1976) 只在提问/切会话五处（1642/1685/1971/2031/2124）调用。知识库回答正文从气泡顶往下长，两屏之后全在视口下方——「流式」最主要的感知收益整个白丢。
- 文件：D:\code\dms_ai\web\src\App.vue
- 改法：加 8 行 `function followStream(){ const el=chatEl.value; if(!el) return; if(el.scrollHeight-el.scrollTop-el.clientHeight>120) return; el.scrollTop=el.scrollHeight }`（阈值 120px 保证用户手动上翻后不被拽回；**用 scrollTop 直接赋值**，不要复用 scrollDown 的 behavior:'smooth'，它会和每帧新内容打架）；在 1543 行 `aiTurn.result = {...}` 之后加时间戳节流的 `void nextTick(followStream)`（模块级 `let lastFollow=0`，间隔 ≥120ms 才调）。
- 验收：提一个超过一屏的知识库问题，断言生成过程中视口停在底部；手动上滚 300px 后继续生成不被拉回底部。

### 5. 深度页 KPI 环比对毛利率类指标用错单位，同一张卡自相矛盾（S，用户可见）

- 为什么：同一张 `.dkpi` 卡里 `comparisonRate`(App.vue:2273-2280) 恒输出「+1.7%」、紧挨着的 `signedComparison`(2285-2292) 对毛利率输出「+0.33 个百分点」；而问数页对同一指标（ResultPanel.vue:352-356 `deltaText` + present.rs:133-142）出的是百分点。用户在两个页面对同一个毛利率读到两个差一个量级的数。
- 文件：D:\code\dms_ai\web\src\App.vue, D:\code\dms_ai\crates\server\src\deep_api.rs
- 改法：前端三行：模板 App.vue:3062 `comparisonRate(cmp)` → `comparisonRate(cmp, t.page.kpi.label)`；函数签名 2273 改 `function comparisonRate(cmp: DeepComparison, label = ''): string`，在 `const pct = comparisonPct(cmp.pct)` 之前插 `if (isGrossMarginLabel(label)) return signedComparison(cmp.change, label)`；3065-3068 的 `.dk-compare-detail` 里那条「变化额」加 `v-if="!isGrossMarginLabel(t.page.kpi.label)"` 避免同句出两遍。根因在 deep_api.rs:339 —— `pct` 恒按 `(current-baseline)/|baseline|*100` 算，没有 present.rs:133-142 的 Percent 分支；产物 markdown 侧（deep_api.rs:469 `comparison_rate_text`、1637 的表格行）仍会出 `%`，那半条归 W5 三端一致同刀做。
- 验收：深度模式问一题「本月毛利率」，卡内变化率与变化额单位一致，且与精简模式同题 ResultPanel 的 `.mc-delta` 文案逐字相同。

### 6. --text-faint 对比度 2.99:1 不过 WCAG AA，被 91 处小字复用（S，用户可见）

- 为什么：实测 #8d95ad 相对亮度 0.3017 → 对 #ffffff 为 1.05/0.3517 = 2.99:1（AA 要 4.5）。落点恰是判断数字可信度要读的那批：KPI 的基期/变化额（ResultPanel.vue:963 `.mc-delta-detail` 10.5px）、深度表行数脚注（App.vue:3862 `.dmore` 10.5px）、板块验收断言（DeepTaskPanel.vue:172 `.tp-task-acc` 10px）。办公室强光屏或投影上基本读不出来。
- 文件：D:\code\dms_ai\web\src\theme.css, D:\code\dms_ai\web\src\ResultPanel.vue, D:\code\dms_ai\web\src\App.vue, D:\code\dms_ai\web\src\DeepTaskPanel.vue, D:\code\dms_ai\web\tests
- 改法：theme.css:7 `--text-faint: #8d95ad` → `#6f7791`（4.45:1）；:25 暗色 `#6b7390` → `#9aa2bd`（6.55:1）；同刀把 :7 的 `--text-muted: #646d87`（实测 5.15:1）压到 `#59617a` 保住三级层次（faint 与 muted 太近会糊成一档）。三处字号 10/10.5 → 11px：ResultPanel.vue:963、App.vue:3862、DeepTaskPanel.vue:172。
- 验收：web/tests 新增一条 node:test：readFileSync theme.css 提 hex 现算 WCAG 对比度，断言 `--text-faint` 对 `--bg-card` 与 `--bg-main` 明暗两套均 ≥4.5:1（改回旧值即红）。

### 7. 「结论与建议」贴着 AI 角标，内容却全是 0-LLM 确定性产物（含权限拒绝裁决）（S，用户可见）

- 为什么：走到 `insightCards` 的 insight 只有四个来源：present.rs:172 的确定性排行/趋势算术、business_lookup.rs:195/215/641 与 entity.rs:956 的 format! 模板（LLM 那两份在 compound.rs:101 / hybrid.rs:148，走 subs 分支且被 App.vue:2363 `dataOnlyResult` 剥掉）。于是 entity.rs:956 的「已按最小权限原则拒绝展示」顶着「AI」角标当结论渲染；用户被训练成对 AI 文本打折，页面上最可信的一行反被贴上最不可信的标签。
- 文件：D:\code\dms_ai\web\src\ResultPanel.vue
- 改法：ResultPanel.vue:646 `<span class="section-kicker">AI</span>` → `<span class="section-kicker">结果要点</span>`；:647 `<h3>结论与建议</h3>` → `<h3>数据要点</h3>`；:650 `.analysis-basis` 文案「基于本次查询结果」→「由本次结果直接计算，未经模型改写」。同刀把这一段挪到 `.kpi-section`（:661-681）之后、`.sales-context`（:683）之前——`<section v-if="insightCards.length">` 整块下移 20 行即可，模板顺序即渲染顺序，无逻辑改动；数字先于对数字的解读。按需 AI 解读（App.vue 的 `.analysis-last`）本来就在最后，位置从此一致。
- 验收：源码断言 `insight-section` 出现在 `kpi-section` 之后且 ResultPanel 内不再出现 `>AI<` 的 kicker；对一题「各省区销售额 TOP10」与一题触发 entity.rs:956 拒绝卡的问句各截图一次。

### 8. 深度页表格与问数页表格是两套视觉语言，且深度页把已废除的前端行数上限又写了一遍（M，用户可见）

- 为什么：同一产品同一类「明细表」两副长相：`.tbl-wrap`(ResultPanel.vue:1015-1035) 单元格 max-width 320 + 省略号 + sticky 行号 + 渲染服务端给的全部行；`.dtable`(App.vue:3852-3862) 全 nowrap + min-width 680 + 无行号 + 客户端硬截 24 行（App.vue:2261 `DEEP_TABLE_PREVIEW_ROWS`）。后者正是 ResultPanel.vue:497-513 那段长注释明令废除的「前端持有行数上限」，用户在深度页拿 24 行当全量。
- 文件：D:\code\dms_ai\web\src\App.vue
- 改法：App.vue:3854 `.dtable th, .dtable td` 去掉 `white-space: nowrap`，改 `max-width: 320px; overflow: hidden; text-overflow: ellipsis;`（与 ResultPanel.vue:1021 逐字对齐）；3853 `min-width: 680px` 降到 `min-width: 0`（列宽交给内容与 max-width）；模板 3119 的 `sec.rows.slice(0, DEEP_TABLE_PREVIEW_ROWS)` 改 `sec.rows`，3122 的 `.dmore` 条件与文案改成与 `rowFoot`(ResultPanel.vue:516) 同句「共 N 行 · 可导出完整 CSV」，删掉 2261 的 `DEEP_TABLE_PREVIEW_ROWS` 常量（净删）。深度板块行数本来就由后端板块查询限着，不需要第二道前端闸。
- 验收：源码断言 App.vue 中 `DEEP_TABLE_PREVIEW_ROWS` 出现 0 次、`.dtable th, .dtable td` 不含 `nowrap`；同一题分别用精简与深度模式跑，两张明细表的截断行为与列宽观感一致。

### ✅[AX123] 9. 知识库流式期间标题与「直接结论」框随半截 markdown 抖动（S，用户可见）

- 为什么：KbAnswer.vue:348 `presented` 对每个 delta 重跑 `presentation()`(:302-348)。标题取第一个 heading（:314）——「## 直接结论」是逐 token 到的，标题会从「直」「直接」跳到「知识库回答」；`summary` 取第一个非列表段落（:322-327），而半截的「-」不匹配 `/^[-*+]\s+/` 排除规则，会被当成结论塞进蓝框再弹掉。生成中标题区反复重排，比不动更糟。
- 文件：D:\code\dms_ai\web\src\KbAnswer.vue
- 改法：KbAnswer.vue:348 改成 `const presented = computed(() => props.streaming ? { title: '知识库回答', summary: '', body: displayMarkdown.value } : presentation(displayMarkdown.value))`。markdown 渲染路径（:350 `html`）一字不动——保留渐进排版，只冻结标题/摘要拆分，收尾时做一次。
- 验收：提一个会返回带 `## 直接结论` 的知识库问题，录屏确认生成过程中 `.answer-title` 文案不变、`.answer-summary` 不出现闪现；完成后标题与结论框正常出现。

### ✅[AX123] 10. 单张 KPI 卡在有图表时横跨整行，28px 数字左挂在 800px 空白里（S，用户可见）

- 为什么：`.kpi-row`(ResultPanel.vue:945) 是 `repeat(auto-fit, minmax(180px,1fr))`——auto-fit 会塌掉空轨道，一张卡就吃满整行。`soloKpi`(:170-173) 只覆盖「无图无表无补充」的纯单指标，一旦同结果带趋势图（最常见的「本月销售额趋势」形态）就落回这条，卡片拉成 889px 宽、116px 高，右侧全空。
- 文件：D:\code\dms_ai\web\src\ResultPanel.vue
- 改法：ResultPanel.vue:945 之后加一行 `.kpi-row:not(.solo) { grid-template-columns: repeat(auto-fit, minmax(180px, 300px)); justify-content: start; }`（`.kpi-row.solo` 的 :969 规则在其后，仍胜出）；:1141 的 `@container(max-width:720px)` 与 :1156 的 `@media(max-width:600px)` 两处已有的 `.kpi-row` 覆写保持不变（窄屏仍要 1fr 铺满）。
- 验收：一题「本月销售额趋势」（单 KPI + 折线图）截图，KPI 卡停在 300px 宽左对齐；一题「本月销售额」（纯单指标）仍走 solo 大卡；一题四指标结果四卡均分不留缝。

### ✅[AX123] 11. 横向滚动表格 hover 时 sticky 列半透明，底下内容穿帮（S，用户可见）

- 为什么：`.tbl-wrap tbody tr:hover .row-index`(ResultPanel.vue:1033) 与 `.dtable tr:hover td`(App.vue:3861) 都把背景换成 `--primary-light`＝`rgba(64,81,211,.08)`，8% 透明度盖在 sticky 定位的首列上，横向滚动时被压在下面的单元格文字直接透出来。同文件 :1029 与 App.vue:3779 已在用 `color-mix`，不是新技术。
- 文件：D:\code\dms_ai\web\src\ResultPanel.vue, D:\code\dms_ai\web\src\App.vue
- 改法：ResultPanel.vue:1033 → `.tbl-wrap tbody tr:hover .row-index { background: color-mix(in srgb, var(--primary) 8%, var(--bg-card)); }`；App.vue:3861 之后补一条 `.dtable tr:hover td:first-child { background: color-mix(in srgb, var(--primary) 8%, var(--bg-card)); }`（首列 sticky 见 3858）。非 sticky 单元格保持 `--primary-light` 不动。
- 验收：取一张 8 列以上的结果表，横滚到中段并 hover 任意行，行号列/首列不透出下层文字；明暗两主题各看一次。

### ✅[AX123] 12. 删两条零消费者的全局样式（S）

- 为什么：App.vue:3641 `.scope-note` 与 :3645 `.tbl-foot` 在 web/ 全仓（src + tests）没有任何 class 引用——scope_note 早已改渲染进 ResultPanel 的 `.foundation-body`（:625-628），行数脚注改成了 `.row-count`(:757)。留着它们会让下一个人以为存在两条并行的呈现路径，而 ResultPanel.vue:883-886 那张「本组件依赖的全局类」清单里并没有它们。删除 > 新增。
- 文件：D:\code\dms_ai\web\src\App.vue
- 改法：删掉 App.vue:3641 与 :3645 两整行（连同 3640/3644 两行说明注释）。
- 验收：`grep -rn 'scope-note\|tbl-foot' web/` 返回 0 行；`npm run build` 与既有 34 条 web 测试全绿；结果卡权限回显与行数脚注截图无变化。

### 13. BiChart 两套调色盘从 JS 常量搬进 theme.css（M）

- 为什么：BiChart.vue:61-64 四个字面色数组是全仓唯一不跟随 token 的颜色源，主题切换靠 :81-82 的 `dark ? DARK : LIGHT` 三元判断自己复刻一遍暗色逻辑；改配色要同时动 theme.css 与一个 .vue 的 script。`cssToken`(:66) 已经是现成读取通道，搬完是净删除。
- 文件：D:\code\dms_ai\web\src\theme.css, D:\code\dms_ai\web\src\BiChart.vue
- 改法：theme.css `:root`(:2-19) 与 `:root[data-theme="dark"]`(:20-32) 各加 `--chart-1..8` 与 `--chart-mono-1..6`（值照抄 BiChart.vue:61-64 现有 hex，零观感变化）；BiChart.vue 删 61-64 四行，:81-82 改成 `series: Array.from({length:8},(_,i)=>cssToken(\`--chart-${i+1}\`, '')).filter(Boolean)` 与同形的 mono 读取——`themeTokens()`(:87-89) 的缓存与 MutationObserver 清缓存链路不动。
- 验收：源码断言 BiChart.vue 中不再出现 `#4051d3` 这类字面色值；一张 8 系列柱图与一张 6 扇饼图在明暗两主题下逐一截图与改前像素比对。


## learning-ledger

前提要更正：任务里说的「四个学习写口今天全是裸写」在 HEAD 上成立，但在我读的这一刻**工作区里已经有账本的第一版（未提交）**：`meta.learn_event` 建表在 ddl.rs:170-185（含 idx_learn_batch / idx_learn_at）、新文件 registry/learn.rs 177 行（log_event / recent_batches / rollback_batch + 两条源码钉板）、四个写口各接了一行（exemplar.rs:243/397/432、memory.rs:65），drift.rs 也已把 `learn_event` 加进 EXEMPT（drift.rs:69）并给回滚分支逐行写了 `ds:any`（learn.rs:37/71/95/110-117）。我实跑了 `cargo test -p dms-semantic --test drift`：3 passed。所以任务的 ①②⑤ 已有骨架，评估必须落在这版骨架的缺口上。

**一、账本今天是「写了但拿不出来」。** 三个写口把 batch_id 传成字面量 `""`（exemplar.rs:244/398/433），第四个传的是 conv_id（memory.rs:66），而 run.rs:873 又把 conv_id 传成 `""`——于是**所有事件的 batch_id 恒为空串**。而 `recent_batches` 的谓词是 `batch_id <> ''`（learn.rs:78 那条 SQL），列表永远空。反面更糟：`rollback_batch("")` 一次匹配全部历史事件，一个请求撤光所有学过的东西。这是本域第一优先级，比任何新功能都靠前。

**二、回滚不是幂等，是一次性。** learn.rs 里 `UPDATE meta.learn_event SET action='rolled_back'` 在 match 之外**无条件执行**：撤失败（PG 抖、目标行已被人删）照样标记，重放就再也撤不回来；它还把 `action` 这一列覆盖掉，insert/update 的原始语义当场丢失。这与本仓 review.rs:160-172 `count_reviewed` 反复钉过的二·AS2「取回 N 行当成处理 N 条」是同一族错误，只是换了张表；而 `undone` 累加 `rows_affected()`，0 行也算「处理过」。

**三、prime-agent 的乐观并发那一半没搬。** 参考实现 apply 时与 baselineState 比对、被改过的条目整条丢弃；本仓回滚是无条件 `DELETE ... WHERE id=$1` / `UPDATE ... SET status=$2 WHERE id=$1`。于是「自动沉淀 → 管理员真实只读执行验证并 enabled（admin_api.rs:288 EX_VALIDATE_OK_SQL）→ 有人回滚那一批」会把人工复核过的语料直接删掉。加这道守卫的成本只有一个 bind。

**四、写口还漏两处，漏的恰是最该撤的两处。** LLM 判 NEGATIVE 直接把语料改成 disabled+invalid 的 `set_ai_review`（exemplar.rs:303-318）零账本；管理员侧 EX_DISABLE_SQL/EX_VALIDATE_OK_SQL/EX_VALIDATE_BAD_SQL/EX_DELETE_SQL（admin_api.rs:285-294）全部绕过 registry 直写自有库，其中 DELETE 是硬删、无任何前值。更该修的是：learn.rs:12 文件头点名「接漏了由 `learn_writes_are_all_ledgered` 钉板抓」，而这条测试**全仓零命中**——文档幻影，与 VIS_PRED 同款。

**五、④ admin 端点一个字都没有**：`recent_batches`/`rollback_batch` 今天零调用者，账本只能 psql 查、根本撤不了；`recent_batches` 还没返回时间列（`min(at)` 只出现在 ORDER BY），结构上答不了「上周二学了什么」。

另有两条硬纪律被这批带破：`rollback_batch` 占 learn.rs:94-139 共 46 行 > D1 的 40；exemplar.rs 已 546 行，越过 D2「>500 必拆」。⑤ 的白名单理由我认可（learn_event 无 ds 列、一次学习可能跨源，按源切反而拼不回一次完整行为），不必再改。

### ✅[AX127] 1. batch_id 落真值：四个写口从调用方拿批次号，空批次号拒绝回滚（M，用户可见）

- 为什么：管理员打开学习台账永远是空列表（recent_batches 的谓词是 batch_id <> ''，而四个写口写进去的全是空串）；反向更危险——POST 一次 rollback 传空串就能撤光全部历史学习
- 文件：D:\code\dms_ai\crates\semantic\src\registry\learn.rs, D:\code\dms_ai\crates\semantic\src\registry\exemplar.rs, D:\code\dms_ai\crates\semantic\src\registry\memory.rs, D:\code\dms_ai\crates\agent\src\run.rs, D:\code\dms_ai\crates\agent\src\review.rs, D:\code\dms_ai\crates\server\src\admin_api.rs
- 改法：批次粒度钉死为「一轮问答 / 一次复核批」，不是会话。①learn.rs::rollback_batch 首行加 `anyhow::ensure!(!batch_id.trim().is_empty(), "空批次号不许回滚（那是全表）")`；log_event 里 batch_id 为空时 warn 一条「该事件不可回滚」。②exemplar.rs 的 save_with_context / save_lesson_candidate / set_lesson_status 各加一个参数 `who: (&str, &str)`（= (batch_id, actor)，用本仓既有的元组别名手法，照 admin_api.rs:40 `type Ident<'a>`），函数体内把今天的字面量 ""/"review" 换成 who.0 / who.1——调用点仍是同一个函数、同一顺序的业务参数，只多一个已在手边的元组。③五个调用点各传：run.rs:928 传 `(&cx.trace_id, &cx.p.login_name)`；run.rs:906 的 spawn 前 clone 一份 trace_id 透给 review::review_failure，再传给 review.rs:73；review.rs:80 review_lessons 开头 `let batch = format!("review-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs());`（std，零依赖）供 :101；admin_api.rs:1183 传 `("sql-edit", &p.login_name)`。④memory.rs:66 的第二参从 conv_id 改成新增的 batch 参数（同③的元组），并把 run.rs:873 传给 save_memory 的 conv_id 由 `""` 改成 `&cx.conv_id`（ctx.rs:62 一直有，meta.memory.conv_id 今天恒空是另一处顺手修的漏）。
- 验收：新增钉板 `every_ledger_write_has_a_batch`：扫 exemplar.rs/memory.rs 源码，`super::learn::log_event(` 之后 2 行内不许出现字面量 `""` 作为第二参；learn.rs 单测断言 rollback_batch 体内含 `ensure!` 且判据字符串含 `batch_id`。手工：跑一轮 llm+repair，`SELECT batch_id,count(*) FROM meta.learn_event GROUP BY 1` 出现该轮 trace_id 且计数≥2。

### ✅[AX132] 2. 回滚标记只在真撤成功时落，且不再覆盖 action 列（S）

- 为什么：今天撤失败（PG 抖/目标行已被删）也把事件标成 rolled_back，那一批从此永久撤不回来；action 被覆盖后连「这条当初是新增还是改状态」都查不出来了
- 文件：D:\code\dms_ai\crates\semantic\src\ddl.rs, D:\code\dms_ai\crates\semantic\src\registry\learn.rs
- 改法：①ddl.rs 在 learn_event 段后加两条幂等 ALTER（形态照 ddl.rs:414-415）：`ALTER TABLE meta.learn_event ADD COLUMN IF NOT EXISTS rolled_back_at timestamptz;` 与 `... rolled_back_by text NOT NULL DEFAULT '';`。②learn.rs::rollback_batch 取事件的谓词从 `action <> 'rolled_back'` 改成 `rolled_back_at IS NULL`；把无条件的 `UPDATE meta.learn_event SET action='rolled_back'` 整条删掉，改成只在 `Ok(r) if r.rows_affected() > 0` 分支内执行 `UPDATE meta.learn_event SET rolled_back_at = now(), rolled_back_by = $2 WHERE id = $1`。③返回值从 `u64` 改成 `pub struct Undone { pub undone: u64, pub skipped: u64, pub failed: u64 }`：rows_affected==0 记 skipped，execute 报错记 failed（仍继续下一条），让端点能诚实地说「撤了 3 条、跳过 1 条、失败 1 条」。
- 验收：learn.rs 源码钉板：rollback_batch 体内不许再出现 `action = 'rolled_back'`，且 `rolled_back_at = now()` 必须与 `rows_affected() > 0` 出现在同一分支（切段断言）。手工：先把某条目标行手动删掉再回滚该批 → 返回 skipped=1、事件行 rolled_back_at 仍为 NULL、再跑一次仍可重放。

### 3. 回滚加乐观并发守卫：人工复核动过的行一律跳过，不硬撤（M）

- 为什么：自动沉淀的语料被管理员真实执行验证并 enabled 之后，任何人回滚那一批都会把这条已复核语料直接 DELETE 掉——学习面回滚反而毁掉人工成果
- 文件：D:\code\dms_ai\crates\semantic\src\registry\learn.rs, D:\code\dms_ai\crates\semantic\src\registry\exemplar.rs
- 改法：把 prime-agent 的 baselineState 比对压成一个 bind。①写侧：insert 类事件的 after 里补上建行时的初始状态——exemplar.rs:245 改成 `Some(json!({"question": question, "status": "pending"}))`、exemplar.rs:399 改成 `Some(json!({"trigger": trigger_tables, "status": "candidate"}))`。②rollback_batch 取事件的 SELECT 加上 after 列（`SELECT id, target_table, target_id, before, after ...`）。③三条回滚 SQL 各加状态守卫，$2 一律绑「账本记下的那个状态」：`DELETE FROM meta.sql_exemplar WHERE id=$1 AND status=$2`、`DELETE FROM meta.pitfall WHERE id=$1 AND status=$2`、`UPDATE meta.pitfall SET status=$2 WHERE id=$1 AND status=$3`（$3 取 after.status）。rows_affected==0 即上一条的 skipped 分支，日志文案写「该行已被人工改动，跳过」。④meta.memory 无状态列，保持无守卫的 DELETE，在分支旁写一行 `// ponytail: memory 是私有层且可再生，没有可比对的状态列；真要守就得存整行快照`。
- 验收：learn.rs 单测钉三条回滚 SQL 各含 `AND status`；手工：沉一条语料 → admin 页 POST /api/admin/exemplars/{id}/status 验证启用 → 回滚该批 → 该语料仍是 enabled 且返回 skipped=1。

### 4. 两个 admin 端点（列最近批次 / 回滚一批）——今天账本零调用者（M，用户可见）

- 为什么：管理员看不到系统学了什么，也没有任何撤回入口：recent_batches / rollback_batch 全仓零调用点，账本只能连 psql 查
- 文件：D:\code\dms_ai\crates\server\src\admin_api.rs, D:\code\dms_ai\crates\server\src\main.rs
- 改法：admin_api.rs 末尾（grant/revoke 之后）加一节：`#[derive(serde::Deserialize, Default)] pub struct LearnQuery { days: Option<i32>, limit: Option<i64>, login_name: Option<String>, role_code: Option<String> }`；`pub async fn learn_batches(State(st), h, Query(q))`：先 `admin(&st,&h,(&q.login_name,&q.role_code)).await?`，再 `dms_semantic::registry::learn::recent_batches(st.owned.pool(), q.days.unwrap_or(7).clamp(1,90), page_limit(q.limit)).await.map_err(db_err)?`，返回 `Json(json!({"batches": rows, "days": days}))`（BatchRow 已 derive Serialize）。`pub async fn learn_rollback(State(st), h, Path(batch_id): Path<String>, Json(req): Json<LearnQuery>)`：admin 门 → `rollback_batch(...).await.map_err(db_err)?` → `affected(u.undone, || format!("批次 {batch_id} 没有可撤回的事件"))?` → 返回 undone/skipped/failed 三个数。main.rs 在 :1533 旁加两行 `.route("/api/admin/learn", get(admin_api::learn_batches))` 与 `.route("/api/admin/learn/{batch_id}/rollback", post(admin_api::learn_rollback))`。days/limit 的夹紧必须在端点做（learn.rs 直接 bind，负 limit PG 会报错——exemplar.rs:416 早就踩过这条）。
- 验收：admin_api 单测照 `no_create_exemplar_route` 的手法用 `include_str!("main.rs")` 反查两条路由真的挂上了（ROUTES 常量守不住 wire 侧，admin_api.rs:19-23 已写明）；纯函数判据钉 days.clamp(1,90) 与 page_limit。手工：无 token curl 两条端点得 401，admin token 得 200。

### ✅[AX133] 5. AI 初筛与管理员复核两条状态变更补进账本（今天完全在账本之外）（M，用户可见）

- 为什么：最该能撤的两条恰好没记：LLM 判 NEGATIVE 把语料改成 disabled+invalid、管理员一键 disable/validate/删除。语料被误判停用后，除了再点一次没有任何「这是哪一批干的」的线索，硬删更是撤不回来
- 文件：D:\code\dms_ai\crates\semantic\src\registry\exemplar.rs, D:\code\dms_ai\crates\server\src\admin_api.rs
- 改法：①exemplar.rs:303 set_ai_review：SQL 前包一层 CTE 取前值（见下一条的同一形状），拿到 old 后 `learn::log_event(pg, batch, "ai-review", "meta.sql_exemplar", &id, "update", Some(json!({"status": old_status})), Some(json!({"status": new_status})))`；batch 由 review.rs:114 review_exemplar 透传（与批次号那条同一个参数）。②admin_api.rs:353-359 与 validate_exemplar/mark_exemplar_invalid 三处直写：执行前先 `SELECT id,status FROM meta.sql_exemplar WHERE id=$1` 拿前值（EX_VALIDATE_GET_SQL:287 已经在取行，加一列 status 即可，零额外往返），执行成功后调一次 learn::log_event，actor=`p.login_name`、batch=`format!("admin:{}", p.login_name)`。③delete_exemplar（:446）：EX_DELETE_SQL 改成 `DELETE FROM meta.sql_exemplar WHERE id=$1 RETURNING question, sql, status`，把整行 JSON 落进 before、action="delete"——**回滚不实现**（re-INSERT 会把 id 换掉），只留证据，在 learn.rs 的分支注释里写明这条天花板与升级路径。
- 验收：扩 admin_api 现有源码守卫：`set_exemplar_status` / `validate_exemplar` / `delete_exemplar` 三个函数体内必须出现 `learn::log_event`。手工：admin 页 disable 一条语料 → GET /api/admin/learn 出现 actor=该管理员的批次 → 回滚该批 → 语料回到 enabled。

### 6. set_lesson_status 改成 CTE 一条语句：先落库再记账，顺带省一次往返（S）

- 为什么：今天是先写账本再 UPDATE（exemplar.rs:429-442）。UPDATE 失败或 0 行命中时，账本里留下一条从未发生过的变更；将来回滚这一批会拿 before 去「还原」一个根本没改过的状态
- 文件：D:\code\dms_ai\crates\semantic\src\registry\exemplar.rs
- 改法：把 exemplar.rs:424-446 的「SELECT 前值 + UPDATE」两条压成一条：`let old: Option<(String,)> = sqlx::query_as("WITH old AS (SELECT id, status FROM meta.pitfall WHERE id = $2) UPDATE meta.pitfall p SET status = $1 FROM old WHERE p.id = old.id RETURNING old.status").bind(status).bind(id).fetch_optional(pg).await?;`（CTE 读的是命令快照＝前值，这是 PG 的标准前值取法）。`None` 分支保留今天的 `tracing::warn!("候选教训复核落库 0 行")`；`Some((old,))` 分支才调 log_event。set_ai_review 用同一形状（上一条）。
- 验收：exemplar.rs 单测切 set_lesson_status 函数体：必须含 `WITH old AS`，且 `log_event` 的字节位置在 `RETURNING` 之后（`body.find("RETURNING") < body.find("log_event")`）；不许再出现两条独立语句。

### ✅[AX127] 7. 把 learn.rs 文件头点名的 learn_writes_are_all_ledgered 真写出来（S）

- 为什么：learn.rs:12 白纸黑字说「接漏了由 learn_writes_are_all_ledgered 钉板抓」，而这个测试全仓零命中——下一个人加第五个写口时不会有任何东西变红，账本会从这里开始漏
- 文件：D:\code\dms_ai\crates\semantic\src\registry\learn.rs
- 改法：learn.rs 的 mod tests 加一条：`include_str!("exemplar.rs")` 与 `include_str!("memory.rs")` 逐行扫，凡出现 `INSERT INTO meta.sql_exemplar` / `INSERT INTO meta.memory` / `INSERT INTO meta.pitfall` / `UPDATE meta.sql_exemplar SET status` / `UPDATE meta.pitfall SET status` 的那一行，往后 25 行内必须出现 `learn::log_event`（窗口取法照 drift.rs:82 的行窗口）；末尾加空转跳闸 `assert!(checked >= 5)`（今天正好 5 处写口，少于 5 说明切漏了）。set_embedding / bump_hits / 两张日志表的 INSERT 不在清单里——它们是观测写入不是学习状态，写进注释以免下一个人误加。
- 验收：反向开枪：注释掉 exemplar.rs:243 的 log_event 调用，这条测试必须红（不红说明窗口切错）。同时把 learn.rs:12 的文件头留着——它从此不再是幻影。

### ✅[AX132] 8. recent_batches 返回时间列：账本要能回答「上周二学了什么」（S，用户可见）

- 为什么：这正是账本立项时写的那句话，而当前 SELECT 里 min(at) 只出现在 ORDER BY、没进结果集——admin 列表拿不到任何时间，结构上答不了这个问题
- 文件：D:\code\dms_ai\crates\semantic\src\registry\learn.rs
- 改法：learn.rs::recent_batches 的 SELECT 加两列 `min(at)::text AS first_at, max(at)::text AS last_at`（`::text` 与 admin_api.rs:282 `created_at::text` 同口径——省掉一个时间类型 feature，零新增依赖），元组类型改成 `(String, String, i64, Vec<String>, String, String)`，BatchRow 加 `pub first_at: String, pub last_at: String` 两个字段。
- 验收：learn.rs 单测钉 SQL 形状含 `min(at)::text`；GET /api/admin/learn 返回体每个批次带 first_at/last_at，与直接查库的 min/max 一致。

### ✅[AX135] 9. 回滚分支抽成纯函数 undo_stmt：D1 40 行 + 现有钉板恒真两件一起修（S）

- 为什么：rollback_batch 现在 46 行（learn.rs:94-139）破 D1；而守它的 rollback_statements_are_compile_time_literals 只断言表名字符串出现在 match 里——meta.memory 在白名单却没有 update 分支，测试照样全绿（DELETE 分支里出现过这个名字就算数）
- 文件：D:\code\dms_ai\crates\semantic\src\registry\learn.rs
- 改法：把 match 那段抽成 `fn undo_stmt(table: &str, has_before: bool) -> Option<&'static str>`（纯函数、返回编译期字面量，ds:any 注释随之搬过去），rollback_batch 只剩「取事件 → undo_stmt → bind → 执行 → 标记」的循环，落回 40 行以内。
- 验收：把现有钉板改成打在 undo_stmt 上的真值判据：`undo_stmt("meta.sql_exemplar", false).unwrap().starts_with("DELETE")`、`undo_stmt("meta.memory", true).is_none()`（写明这是刻意的：memory 没有可还原的状态列）、`undo_stmt("meta.term", false).is_none()`（白名单外一律 None，不许拼串）；再保留一条源码守 `!body.contains("format!")`。

### ✅[AX135] 10. 删掉 learn_event.trace_id 列（与 batch_id 恒等的一列白存）（S）

- 为什么：log_event 把同一个值 bind 了两次（learn.rs:42 与 :49），trace_id 永远等于 batch_id。多一列就多一处将来会漂的语义，而它今天一个读者都没有
- 文件：D:\code\dms_ai\crates\semantic\src\ddl.rs, D:\code\dms_ai\crates\semantic\src\registry\learn.rs
- 改法：ddl.rs:170-183 的建表语句里删掉 `trace_id text NOT NULL DEFAULT ''` 一行（`CREATE TABLE IF NOT EXISTS` 对已建的库不生效，现网留一列空字符串无害，不写 DROP COLUMN——那是不可逆操作，不值得为一列冒险）；learn.rs 的 INSERT 列清单去掉 trace_id、占位符从 $8 收回 $7、删掉第二次 `.bind(batch_id)`。真需要「批次内定位到某一轮」时，batch_id 本身就是 trace_id。
- 验收：learn.rs 单测钉 INSERT 的 `$` 占位符个数与 bind 次数相等（`sql.matches('$').count() == 7`），防下次改列时再漂一位。

### ✅[AX136] 11. exemplar.rs 拆出 registry/pitfall.rs：546 行已越过 D2「>500 必拆」（M）

- 为什么：账本这一批把它从 513 推到 546。它是全仓改得最勤的文件之一（语料召回 + 教训读写 + 两张日志表），越界后每次改都要在 900 行级的 review 面里找
- 文件：D:\code\dms_ai\crates\semantic\src\registry\exemplar.rs, D:\code\dms_ai\crates\semantic\src\registry\pitfall.rs, D:\code\dms_ai\crates\semantic\src\registry\mod.rs, D:\code\dms_ai\crates\agent\src\review.rs
- 改法：把 meta.pitfall 的三个写读函数（save_lesson_candidate / candidate_lessons / set_lesson_status，exemplar.rs:363-447）连同它们的 doc 与相关单测整段搬进新文件 registry/pitfall.rs（约 -90 行，exemplar.rs 落回 450 甜区内），mod.rs 加 `pub mod pitfall;`；调用点只有三处：review.rs:73/82/101 的 `exemplar::` 前缀改 `pitfall::`（同一行、同参数）。不加 re-export（那等于留两条路，下一个人又会从旧名进）。注意与读侧 recall/pitfall.rs 不同层（一个是 registry 写口、一个是召回渲染），文件头各写一句互指。
- 验收：`cargo test -p dms-semantic` 与 `-p dms-agent` 全绿；drift.rs 的 every_meta_recall_is_ds_scoped 仍绿（src/** 全树扫描自动覆盖新文件，无需改清单）；`wc -l` 两个文件都在 450 以内。


## accuracy-next

逐条开代码复核了两份方案里点名的八族，结论是**四族已被本轮修复吃掉、四族原样还在，另外抓到一条本轮修复自己带出来的新硬失败**。

已解决、不要再提：①`metric_proved` 八族封闭 —— `crates/agent/src/intent.rs:1900` 的 `other =>` 臂已改成「表面词出现在聚合投影里即证明」，市场费用/开票金额/客单价那批不再恒 false；②覆盖闸一票否决 —— `intent.rs:1611 blocking()` / `:1617 needs_review()` 两级已落地，`hits.rs:108,112`、`run.rs:670,681` 两处调用点都按分级走；③地区值 `folded_eq` 精确等值 —— `intent.rs:1877 has_value` 已改互为子串（日期仍走 `has_value_exact`，正确）；④few-shot 无相似度下限 —— `crates/semantic/src/registry/exemplar.rs:26 FEWSHOT_MIN_SIMILARITY=0.15` 已在 SQL 里，语义缓存那侧 `answerers/cache.rs:22 MAX_DIST=0.12` 本来就有，这条不对称已消失；⑤Doris 会话超时 —— `crates/connector/src/mysql.rs:466-471` 已有 `after_connect` + `query_timeout=45`（但 `:457 .timezone(None)` 那半没做，见 #14）。

原样还在、且证据比方案写的时候更硬：**59 表编译期目录仍是读取侧的硬白名单**。`recall/schema.rs:54-62 catalog_table_filter` 把 `ASSETS` 拼成 SQL 的 `table_name = ANY($4)`，三路召回全过它；`recall/metric.rs:98-104` 一个静默 `.filter(catalog_allows_metric_record)`，零日志零计数。而 `warehouse_catalog.rs` 只有 59 条 `asset!`，`t_shop_inspection_records` / `t_goods_category` / `t_employee` / `t_invoice_apply_detail` / `t_warehouse` / `t_customer_device_ledger` / `t_activity_*_fee` 一条都不在里面 —— 于是 `ops_caliber.rs:412` 播种的「巡店省区/巡店城市」维度、`ops_caliber.rs` 的 7 条巡店指标、`seed.rs:483` 那条注释里写着实测虚高 36% 的商品分类值域，全部在读取侧被丢掉，运行时看不出任何异常。附带一条：`registry/mod.rs:353 TABLE_PREFIXES` 没有 `scm_`，而 `warehouse_catalog.rs:334` 恰好登记了 `scm_warehous_manage` —— `source_refs` 返回空 → `source_uses_warehouse_catalog`（`mod.rs:394` 的 `!refs.is_empty()`）恒假 → 库存指标必被 `catalog_allows_metric`（`mod.rs:625`）拒。这是一处一改复活四族的杠杆。

省区四份真相源一字未动：`ops_caliber.rs:31 province_region` 的 REGEXP CASE 里没有上海、没有海南；`:72 inspection_valid` 把 `(province_region(...)) IS NOT NULL` 当有效性过滤 —— 上海门店的巡店记录被整批静默排除；`:90 region_of` 是第三份省名词表（同样没有上海）；权威的 `warehouse_catalog.rs:396 shop_business_region_for_province` 有上海→浙江省区、海南→广东省区两个特例，零消费者。`allowed_dimensions` 也一字未动：唯一消费者是 `compose/path.rs:142`，被 `fastpath_intent.rs:469/862` 两处确定性装配门调用，LLM 路只在 `recall/metric.rs:190` 拿到一句「允许维度：…」的提示词软约束。深度报告子问 `deep_api.rs:2187` 仍是 `crate::ask(...)` 吃裸字符串，`validate_plan`（`:3613-3626`）仍只校验 2-60 字与 chart 枚举。Doris EXPLAIN `mysql.rs:881 Ok(Ok(_)) => Ok(None)` 整条计划丢弃。`meta.failure_log` 全仓仍零 SELECT（`exemplar.rs:438` 是唯一非 DDL 引用）。

**新发现（本轮 P0-② 的副作用）**：`intent.rs:1593-1596` 那条降级护栏 —— `unverifiable` 非空且投影无聚合就升格 `conflicts`（=硬阻断）。它对聚合题是对的，但 `intent.rs:1556` 未登记的筛选名一律落 `unverifiable`，而 `filter_columns`（`:1834`）仍是写死五族。两条一叠：任何**明细型**问句只要带一个五族之外的筛选（渠道/品牌/活动类型/业务类型），就是 unverifiable + 无聚合 → conflicts → 硬失败。这批题在两级闸之前也是失败，但现在失败原因被记成「冲突」，且方案 W2#2 只写了「改读维度注册表」，没写这条叠加。

### 1. 编译期语义目录补 12 张 ODS 表 + TABLE_PREFIXES 加 scm_，先加门禁再收敛（M，用户可见）

- 为什么：「本月各省区的巡店次数」「手抓饼这个分类今年卖了多少箱」「本月库存量」这三族问句今天拿不到任何 schema 卡/指标卡/值域卡：表召回被目录白名单挡在 SQL 层，指标被静默 filter 掉。用户看到的是 LLM 猜表、或分类题按 sku_name 匹配虚高 36%。
- 文件：crates/semantic/src/warehouse_catalog.rs, crates/semantic/src/registry/mod.rs, crates/semantic/tests/drift.rs
- 改法：① drift.rs 新增 every_seeded_declaration_survives_the_catalog_gate：把 seed.rs 的 KW_FORCE/EDGES/TABLE_SCOPES、seed_defs.rs 的 METRICS/DIMENSIONS、ops_caliber.rs 的 metrics()/seed_dimensions 逐条喂进 catalog_allows_table/catalog_allows_metric_record，任一被拒即红（判据与运行时同源，不抄第二份）。② 红了之后按 asset! 现有形态补进 warehouse_catalog.rs 的 ASSETS：t_shop_inspection_records / t_goods_category / t_employee / t_warehouse / t_invoice_apply_detail / t_customer_device_ledger / t_activity_{freight,material,other,tasting,venue}_fee / t_account_bill_{header,detail} / t_regions；已下线的 t_market_total_expense 走另一半——删掉它的种子行与边，不要补进目录。③ registry/mod.rs:353 的 TABLE_PREFIXES 追加 "scm_"（同文件 :110 的 SOURCE_ASSET_LIVE_PRED 正则是它的拷贝，同刀加 scm_[A-Za-z0-9_]+）。④ recall/metric.rs:98 的静默 .filter 加一句 tracing::debug 计数（丢了几条），否则下次漂移仍然无声。
- 验收：新门禁测试从红转绿；regression.py 全量无回退；新增回归题「本月各省区的巡店次数」route 不再是 llm、「手抓饼这个分类今年卖了多少箱」SQL 必须 JOIN t_goods_category 且按 category_name 过滤、「本月库存量」SQL 含 inventory_status='ZP'。

### ✅[AX124] 2. 降级护栏把明细型问句打成硬阻断：护栏条件改成「用户要了指标才判聚合」（S，用户可见）

- 为什么：「本月线下渠道的订单明细」这类带非五族筛选的明细题，今天走 unverifiable→conflicts→blocking，用户拿到 422「暂时无法完成本次问数」。这是 AX117 两级闸的副作用，方案里没记。
- 文件：crates/agent/src/intent.rs
- 改法：intent.rs:1593-1596 的 `if !report.unverifiable.is_empty() && !projections_have_aggregate(&proof.projections)` 加一个前置合取项 `!intent.metrics.is_empty()`——护栏本意是「模型压根没算用户要的指标」，用户没要指标时它就不该开火。requested_detail 已有独立判据 detail_shape_proved（:1497）兜着形状。同刀把 filter_columns（:1834）的 None 臂注释改成事实：未登记筛选名只降 review。
- 验收：intent.rs 单测三条：(metrics=[销售额], 投影无聚合) 仍 blocking；(metrics=[], filters=[渠道类型=线下], requested_detail, 明细投影) → blocking()==false 且 needs_review()==true；(metrics=[], 投影为 SELECT *) 不因此放行到 extra/missing 之外。regression.py 新增「本月线下渠道的订单明细」断言 status=succeeded。

### ✅[AX134] 3. 省区映射收敛成一份：CASE 由 shop_business_region_for_province 生成（M，用户可见）

- 为什么：上海、海南两地的门店巡店记录被 inspection_valid 的 IS NOT NULL 整批静默排除——「本月上海的巡店次数」恒 0，「今年各省区巡店次数」全国合计偏低，且没有任何提示。
- 文件：crates/semantic/src/ops_caliber.rs
- 改法：新增 fn standard_region_pairs() -> Vec<(&'static str,&'static str)>：遍历 warehouse_catalog 的省份表调 shop_business_region_for_province，再剥「省区/大区」后缀得到 23 值域；ops_caliber.rs:31 province_region 的手写 REGEXP CASE 改由该 pairs 遍历 format! 生成；:90 region_of 的第三份省名词表同样由 pairs 生成（同一份数据两种形态）。activity_region(:49) 里那份 23 值 IN 列表也从 pairs 拼。业务口径确认项：上海归浙江省区、海南归广东省区在运营看板下同样成立——权威表 t_shop_province_department_mapping 已这么记（seed.rs:484 的 note 逐字写了这两个特例）。
- 验收：ops_caliber.rs 单测：对 warehouse_catalog 的每个省份断言 standard_region 有值（港澳台除外）；显式断言上海→浙江、海南→广东；断言生成的 23 值域集合与 activity_region 的 IN 列表逐字相等（今天是两份）。回归题「本月上海的巡店次数」从 0 变非 0。

### ✅[AX125] 4. Doris EXPLAIN 的执行计划别丢：全分区扫描判成可 repair 的缺时间谓词（S，用户可见）

- 为什么：语法合法但要扫全表的查询今天一路跑到 30s EXEC_TIMEOUT 才失败，用户等满半分钟拿到一句超时。计划文本已经付过一次往返了。
- 文件：crates/connector/src/explain_plan.rs, crates/connector/src/mysql.rs
- 改法：新建 explain_plan.rs（纯函数、无 IO，避开 mysql.rs 已 1641 行超 D2）：pub fn scan_verdict(plan: &str, total_floor: u32) -> Option<String>，按行扫 partitions=(\d+)/(\d+)，已扫==总数 且 总数>=total_floor(8) 时返「计划显示全分区扫描 N/N，请补时间过滤」。mysql.rs:881 的 `Ok(Ok(_)) => Ok(None)` 改成把行集拼成文本喂给它。不改 Source::explain 签名——source.rs:141-143 的 Option<String> 语义（Some=数据库判定有问题、可拿去 repair）正好适配，run.rs:748-750 的 explain-fail 留痕与 repair 轮零改动接住。
- 验收：explain_plan.rs 单测：一段真实 Doris EXPLAIN 文本（partitions=1358/1358）→ Some 且文案含 1358；partitions=3/1358 → None；总数 4（小表）→ None。mysql.rs 现有的 after_connect 守卫测试旁边加一条断言 :881 不再是 Ok(None)。

### 5. 深度报告板块问句继承父问已 grounding 的地区/实体（validate_plan 那一刀，不动 ask 链）（M，用户可见）

- 为什么：「山东省本月售后单情况」的非销售板块被当成全新独立问题重新理解，SQL 里一个省区谓词都没有——用户拿到一份看起来完整、其实有一两块是全国数的报告，且缺席的板块在报告里无声消失。
- 文件：crates/server/src/deep_api.rs
- 改法：validate_plan(:3613) 加第二形参 `grounded: &[String]`（父意图里 Grounded/Resolved 的 region/entity surface，调用侧从 PreparedAsk 取）：对每条 section，surface 不在 s.question 里就拼到问句头部；拼进去与板块自身维度冲突（问句已含另一个同族地区词）则整条淘汰。execute_plan_sections 的 .flatten()(:2172-2176) 改成保留失败位并在报告里出一行占位说明——note_section_state 已经把 failed 写进进度面板，报告本体还是静默的。**不做** sub_ask 改吃 PreparedAsk 那半（那是 L，且要动 5 个调用点）；先用这刀把「板块丢地区」这个错答案止掉。
- 验收：deep_api.rs 单测：grounded=["山东省"]，plan 返 [{"本月各商品销售额"},{"山东省各省售后单数"}] → 第一条被补成含「山东省」、第二条原样保留；grounded=["山东省"] + 板块问句含「广东」→ 该条被淘汰。deep_contract_eval.py 加一题断言每个板块 SQL 都含省区谓词或该板块带占位说明。

### 6. allowed_dimensions 加一条 CaliberRule，让白名单也管得住 LLM 路（M，用户可见）

- 为什么：越是没审定的指标×维度组合越会落到自由 SQL 那条路（确定性装配门一挡就回落），于是白名单在真实流量上等于不存在——用户拿到一个按未验证维度切出来的数，收据仍是 verified。
- 文件：crates/kernel/src/sql/caliber.rs, crates/semantic/src/registry/caliber.rs
- 改法：CaliberRule 加变体 AllowedDimensions{metric, allowed: Vec<String>, human}：从 AST 取顶层 GROUP BY 列（复用 NoFanoutJoin/RequireCols 已在用的那条提取路径），经 meta.dimension 的 expr/别名反查成维度名，不在 allowed 即违规，human 写「指标 X 未验证按 Y 切分，允许维度：…」。构造侧在 registry/caliber.rs 的规则装配处从已加载的 MetricPolicy 直接造（load_metric_policies 已在链上）。防误伤三条不可省：allowed 为空或含 '*' 一律不判、分区时间列豁免、判词进 repair 回炉不直接 fail closed。不改 recall_dimensions 签名。
- 验收：caliber.rs 单测五条：allowed 内维度绿 / allowed 外维度红且 human 点名 / 含 '*' 不判 / 空集不判 / 分区时间列不判。evaluation.py 38 题逐题结果集不变，新增一题「按未验证维度切分」断言 caliber_note 非空。

### ✅[AX124] 7. 复合答案：一个子问失败不再整轮 422，且容器补上聚合收据（S，用户可见）

- 为什么：两个 subgoal 的题，其中一个失败时用户连另一个已经查出来的子结果都看不到；复合容器的 trust 与 intent_summary 恒 None，前端「问题理解与结果依据」整块空白。
- 文件：crates/agent/src/ask.rs, crates/agent/src/ctx.rs
- 改法：ask.rs:427 的 `one(question.clone()).await?` 改 match：Err 收进 failed 列表、用还活着的 compound::missing_note 点名，Ok 照常进 subs；全部失败才上抛。AskResult::compound(ctx.rs:254) 加两个形参接住 trust 与 intent_summary（把 main.rs:2425-2470 的 hybrid_intent_summary 搬进 agent 复用，顺带消掉 server 手工拼 coverage JSON 那段），填掉 :275/:281 两个写死的 None。
- 验收：ask.rs 单测：两个 Data subgoal 的 IntentAttempt → 返回的 compound trust.is_some() 且 intent_summary.is_some()；一个子问返 Err → 另一个子结果仍在 subs 里且 caliber_note 点名失败的那个。

### ✅[AX125] 8. 问句切片改用 Query 向量：与整句同空间，且不再挂语料侧熔断槽（S，用户可见）

- 为什么：元素卡（指标/维度/码值）的召回阈值 STRICT=0.35/LOOSE=0.5 与 DS_MAX_DIST 都是拿 query 向量标定的，今天却拿 passage 向量去比——口语化问法整体召回漂移，且一次知识库入库失败会掐掉 5 分钟的切片召回。
- 文件：crates/agent/src/gather.rs, crates/connector/src/embed.rs
- 改法：gather.rs:86 的 `embed.embed_passages(&slice_texts)` 改 `embed.embed_queries(&slice_texts)`（函数已存在，embed.rs:173）。同刀在 connector 侧关掉误用入口：删掉 embed_passages/embed_queries 两个同形包装，只留 pub embed_batch(&self, texts, mode: EmbedMode)（embed.rs:178 的 embed_batched 直接提 pub），五个调用点必须显式写模式——少两个函数，且「随手挑那个批量的」这个错犯不出来。
- 验收：gather.rs:1041 的存在性断言从 contains("embed_passages") 改成 contains("EmbedMode::Query")；embed.rs 单测：Query 模式批量只动 query 熔断槽。regression.py 全跑对拍改前后，重点看依赖元素卡的口语化问法组。

### ✅[AX126] 9. 语义召回降级写进 trust：口径卡缺席不许还显示 verified（M，用户可见）

- 为什么：PG 抖一下 → 指标卡缺席 → LLM 拿不到销售额的口径表达式/时间列/去重键 → 数字按错口径算出来 → 前端仍是 verified/high。这是「答错了还很自信」的唯一结构性来源。
- 文件：crates/agent/src/gather.rs, crates/agent/src/ctx.rs, crates/agent/src/run.rs
- 改法：BudgetReport(gather.rs:317，notes 恒空的死件) 改成 RecallHealth{degraded: Vec<&'static str>, kept_recalled, kept_counters}，12 处 map_err(warn) 各追加一次 push（字面量与 warn 文案同源）；ContextSummary(ctx.rs:1030-1050) 删掉恒 false 的 summary_used、恒空的 trimmed 与只有测试生产者的 TrimNote，换成 degraded；run_llm 在指标/口径类降级非空时把一行写进 caliber_note → ctx.rs:372 的 risk 判据自动生效，trust 降 review、checks 多一行「本轮业务口径卡缺席，数字未经口径素材约束」。
- 验收：gather.rs:1023-1029 的等式测试扩成三元（unwrap_or_default 条数 == warn 条数 == degraded.push 条数）；ctx.rs 单测 risk=true → trust=="review"；源码守卫断言 run.rs 里存在把 degraded 写进 caliber_note 的那一行，且全仓不再出现 TrimNote。

### 10. LLM 一次重试 + 让 LlmError::Api 这个死变体真正被构造（S，用户可见）

- 为什么：供应商一次 429 就是一次回答失败，用户看到「LLM 请求失败（HTTP 429）」；而 kernel 里 Api{status,body} 变体除测试外全仓零构造，排障时限流与模型下线在日志里同形。
- 文件：crates/server/src/llm.rs, crates/kernel/src/llm.rs
- 改法：llm.rs:310-315 的非 2xx 分支把 status 带出来（映成 LlmError::Api{status, body 截断 512}）而不是 anyhow::bail! 成字符串；在 chat 这唯一出口加**一次**重试：matches!(status, 429 | 500..=599) 或 reqwest 超时 → sleep 800ms 重发一次，第二次失败照旧上抛。不做退避框架、不做次数配置、不搬 llm.rs 进 connector。
- 验收：llm.rs 用现成 TcpListener 桩：第一次 429、第二次 200 → chat 返 Ok 且只记一次 usage；第一次 400 → 立即 Err 且不重发（桩里数连接数）。

### 11. failure_log 读回来：只有重复出现的失败才配烧一次复盘，且复盘素材不许带权限片段（S）

- 为什么：用户侧感知是「同一个坑反复踩、系统从来学不会」：一次性抖动和第 7 次重复失败在系统眼里完全一样。附带一条 I4 漏面：复盘 prompt 今天喂的是注入后的 SQL。
- 文件：crates/semantic/src/registry/failure.rs, crates/agent/src/run.rs, crates/agent/src/review.rs
- 改法：新建 registry/failure.rs（不进已很大的 exemplar.rs），一个函数 pub async fn failure_streak(pg, ds, kind, err_class:&str, days:i32) -> i64，SQL 为 SELECT count(*) FROM meta.failure_log WHERE ds_id=$1 AND kind=$2 AND left(error,60)=$3 AND created_at >= now()-$4::int*interval '1 day'。run.rs:902-907：spawn 前先查一次，streak<2 只落日志不调模型（省掉大部分 fast 复盘），>=2 才调 review_failure 并把次数拼进 user 段；同刀把 :905 的 `scoped.wire().to_string()` 改成闸门前的候选 SQL（与 AX118 修 HITL sql-edit 同一条纪律——行级权限条件不进 ds 级共享 prompt）。
- 验收：failure.rs 纯 SQL 形状单测（drift.rs 的 ds 谓词守卫本来就扫得到）；run.rs 源码守卫：spawn review_failure 那段必须出现 failure_streak，且不得出现 scoped.wire()。手工：连续两次制造同一 exec-error，第一次日志无复盘、第二次有。

### 12. RowSet 带上 truncated：被 ds 策略压到 50 行的结果不再冒充完整结果（S，用户可见）

- 为什么：effective_limits 会把生产能力压到 50 行、DsPolicy.max_rows 可压到任意值，此时 ctx.rs:312 的 `row_count >= MAX_ROWS` 为假 → 前端脚注只写「50 行」，既不显示已截断也不给续读——几千行的结果被呈现成完整答案。
- 文件：crates/connector/src/source.rs, crates/connector/src/mysql.rs, crates/connector/src/postgres.rs, crates/agent/src/ctx.rs
- 改法：RowSet 加 pub truncated: bool；两个 to_table 在 rows.len() > max 时置真（mysql.rs:842 那处已经拿着 rows 和 max）；ctx.rs:312 的 `truncated: row_count >= MAX_ROWS` 改成 `rs.truncated || row_count >= MAX_ROWS`，truncation_note 同源。前端零改动（truncated 已在契约里）。
- 验收：connector 单测：to_table(&rows_of(50), 50).truncated==true、to_table(&rows_of(3), 50).truncated==false；agent 单测：ds 策略 max_rows=20 时取回 20 行 → truncated 且 note 非空。ctx.rs:1712 既有 table_answer_shape_and_truncation 扩一条。

### 13. t_employee 从 Global 移到 Scoped（Java EmployeeDao 有 @DataScope）（S，用户可见）

- 为什么：今天任何受限账号都能拿到全量花名册、部门归属、登录名——SENSITIVE_COLS 那 9 词只挡凭据列。方向与本轮已修的两张 dnf 表 fail-open 是同一族。
- 文件：crates/policy/src/builtin.rs, crates/semantic/src/ops_caliber.rs, crates/policy/tests/inject_tests.rs
- 改法：builtin.rs:119 的 global 循环里删掉 "t_employee"，改用既有 helper owner_only("employee_id", Ids)（t_invoice_apply_header 正在用）。t_employee_department/t_department 是纯组织维表、Java 无注解，保持 global。**前置必做**：ops_caliber.rs:72 inspection_valid 里那条 `NOT EXISTS (... t_employee oe JOIN t_position op ...)` 子查询转 scoped 后会被注入员工过滤，等于「职位排除只对自己可见的员工生效」＝口径被静默放宽——该子查询走 via 豁免或先物化排除名单，两者都要在同一提交里。
- 验收：builtin 计数测试改名 builtin_table_counts_by_kind 并断言 matches!(m.get("t_employee"), Some(TableRule::Scoped(_)))；inject_tests 加 rewrite("SELECT actual_name FROM t_employee", sets(ids=[7])) 含 t_employee.employee_id in (7)；ops_caliber 单测断言巡店有效性子查询的 SQL 与改前逐字相同；regression.py 全量跑，改判题号写进提交信息。

### 14. Doris 会话钉住时区：CURDATE() 与 PG 侧「今天」同一口径（S，用户可见）

- 为什么：nl/time.rs 里近两百处 CURDATE()/NOW() 跑在一个从没被钉过的服务端时钟上，而 daily_digest.rs:28-31 的「今天」被显式钉成 Asia/Shanghai——跨日边界两侧「今天的销售额」可以给出不同的窗口。
- 文件：crates/connector/src/mysql.rs
- 改法：mysql.rs:466-471 那个已存在的 after_connect 语句数组里追加一条 "SET time_zone = '+08:00'"（用固定偏移不用地名，Doris 各版本地名表不一致；失败照旧被同一段 .ok() 语义的 debug 吞掉，不阻断建连）。:457 的 .timezone(None) 保持——那是 sqlx 握手项，不是会话变量。
- 验收：mysql.rs:1346 旁边那组源码守卫加一条 assert!(body.contains("time_zone"))；手工在跨日边界前后对同一句「今天的销售额」跑两次，SQL 里的日期与服务器 date +%F 一致。


## 对抗验伪

- **READY** 暗色品牌渐变 1.01:1——侧栏 logo 在暗色下是隐形的 —— 全部实测复算一致：theme.css:23 暗色 --brand-ink 是 #161c33→#2b3673，对 --bg-card #1a1e2b 为 1.01:1 / 1.49:1；提议的 #aeb8ff→#e8ebf6 实测 8.78:1 / 13.95:1。App.vue:3405 的 -webkit-text-fill-color: transparent + var(--brand-ink) 与 :3681 的 .metric-card::before 顶条确认是仅有的两个消费者。一行改动、零依赖、不动亮色。
- **READY** --text-faint 2.99:1 不过 AA（并订正 W6#4 给错的两个值） —— 我用 WCAG 公式独立复算，全部数字逐位吻合：faint #8d95ad = card 2.99 / main 2.81 / sunken 2.55；提议 #67708c = 4.91/4.62/4.50/4.19，muted #545c76 = 6.63/6.23/6.07/5.65，层次不倒挂。对 W6#4 的两处订正也成立：#6f7791 对 --bg-main 实测 4.18（过不了它自己写的 ≥4.5 断言），暗色 #9aa2bd 6.55 > muted #8b93ad 5.44 确实把三级层次倒挂。theme.css:7/:25 行号准确。注意本条与 result-presentation 区那条同名提案是同一处改动、值给错了——以本条为准，那条不要另开一刀。
- **READY** 暗色主色按钮上的 #fff 只有 3.14:1（23 处） —— `grep -c 'color: #fff' *.vue` = 23，与提案逐行核对全中（App.vue:3410/3455/3474/3552/3634/3657/3746/3769/3849/3875、DataMapPanel:1032/1033、KbAnswer:655/662、KbDocPreview:772、KbEval:511、KbGraph:1084、KbMindmap:861、KbPanel:3349/3354、ResultPanel:1115、SkillsPanel:285/286）。#fff 对暗色 --primary #7b89f0 实测 3.14:1，提议的 #11141d 实测 5.85:1。KbPanel:3354 .danger-btn 底色确是 var(--error-text) 不是 primary，单独留 #fff 的判断正确。新增一个 token、零依赖。
- **READY** BiChart 单色阶浅端 1.43:1——饼图图例色块在白卡上看不见 —— BiChart.vue:63/64 两条数组行号准确；现值 #aeb6f2/#d1d6f8 对白卡实测 1.95/1.43。提议的 LIGHT_MONO 六阶实测 9.30/7.37/5.86/4.68/3.84/3.04（全过 3:1），DARK_MONO 对 #1a1e2b 实测 4.27→12.08，与提案写的一字不差。:167-168 的「>5 走滚动图例、不画扇区标签」判据核对属实，色块确是唯一映射。
- **FIX_SHAPE** TOP 收纳静默截断：200 行只画 10 根柱，标题一个字不说 —— 问题真实：chartCaption 在 ResultPanel.vue:369，只出「各X贡献与排名」；BiChart.vue:151-155 的 TOP 收纳确实静默截断。但调用点清单错了：:723 是**主结果**的 compositionCharts（跟 :702 一样用 props.result.rows），不是补充区；真正的补充区调用在 :811 与 :824（不是 :814/:827）。照写会把 supplemental.rows 传给主图，行数说反。
  - 修正：chartCaption(block, view = props.result.view, rows = props.result.rows)：:702/:723 两处主结果调用**一个字不改**（吃默认值），只把 :811/:824 改成 chartCaption(b, supplemental.view, supplemental.rows)。另：verification 里「搬进 W6#12 的 result-view.ts」把本条锁死在另一条尚未落地的提案上——先把 node:test 直接打在 ResultPanel.vue 的源码断言上，搬文件那刀单独排。
- **READY** rows 变空时 BiChart 留着上一张图不清 —— BiChart.vue:148 的 `if (!props.rows.length || !props.y.length) return` 行号准确；:392-395 的 watch 确实盯 props.rows 会重跑 render，所以 rows→[] 时的确是「重画一次、早退、旧图还在」。chart-card 用的是 index key（:698 `trend-${bi}`），同一 ResultPanel 换结果会复用同一个 BiChart 实例，路径可达。.chart-state 样式在 :1003，行号准确。
  - 修正：只做 chart.clear() 那一行。ResultPanel 那半（补 `<p class="chart-state">本轮无数据可绘</p>`）可省：:578-580 已有 .empty-hint「未找到数据」覆盖整个 0 行结果，再加一条是同一件事说两遍。
- **READY** 暗色首屏闪白 + 不认系统偏好 + 原生控件不跟主题 —— 三条前提逐条查实：全仓（含 index.html）`prefers-color-scheme` 与 `color-scheme` 命中 0 次；App.vue:1105 确是 `localStorage.getItem('theme') || 'light'`；applyTheme 在 :1718，挂载后才写 data-theme。index.html 目前 head 里只有 charset/viewport/title，插 4 行内联脚本没有任何冲突。零依赖，三处改动都具体到能写。
- **FIX_SHAPE** 遮罩层 4 个硬编码值 + 暗色模态零分离（W6#10 的暗色缺口） —— 主体属实：7 处 rgba(17,24,39,.38)（App:3592/3750/3921、DataMapPanel:1014、SkillsPanel:257、SqlAuditPanel:273、UsagePanel:174）+ App:3864 的 .42 + KbPanel:2975 的 rgba(16,22,43,.48) + KbPanel:3357 的 .42；五个同义 spin keyframes（dnSpin/dmSpin/skSpin/saSpin/upSpin）也逐个核到。三个缺口：①**漏了 KbDocPreview.vue:736 的 rgba(16,22,43,.55)**——第 5 个 scrim 值，照原方案做完，verification 里「rgba(16, 22, 43 各 0 次」当场假红；②KbPanel:3357 .confirm-mask 是 `position: absolute`（面板内确认层，z-index:2），套不进 `.ui-mask{position:fixed}`；③「顺手给 SkillsPanel/SqlAuditPanel/DataMapPanel 补 scoped」是无关的搭车改动，App.vue 的 <style> 本身没 scoped、全仓靠全局类互相命中（ResultPanel.vue:880-883 那张清单就是证据），一刀补 scoped 是静默破损的高发处。
  - 修正：scrim 清单加 KbDocPreview.vue:736，共 11 处；.confirm-mask 单独给一条 `.ui-mask.inset{position:absolute}` 或干脆保留原样只换 var(--scrim)；补 scoped 那句删掉，要做另开一条并逐类核消费者。keyframes 合一与 --scrim/--bg-elevated 两个 token 照原案做。
- **FIX_SHAPE** 触控热区全线 24-30px，删除键紧贴查看键 —— ①②站得住：.hi-del/.hi-trace/.hi-clear 在 App.vue:3525-3527，都是裸 button 无尺寸；:3534 的 `@media (hover:none)` 确实只带了 hi-trace/hi-del、漏了 hi-clear，提案这个观察是准的；.btn-sm 高 26px 在 :3881。③与 result-presentation 区的「移动端 chrome 收口」是**同一刀**，且本条的选择器更脆：`.topbar .btn-sm:not(.mobile-kb):not(.mobile-weekly){display:none}` 会连「+ 新会话」（:2694）和「退出」（:2695）一起藏掉，而那两个不是工具入口。另：assessment 说「820px 块里一条尺寸规则都没有」不确——:3922-3936 有 .bubble/.pv-hd/.bi-focus 等一串。
  - 修正：本条只留 ①（.btn-sm/.btn-icon/.btn-mini/.pill/.ask-opt 的 min-height:44px + padding-inline）与 ②（三个 hi-* 做成 44×44 常显，并把 :3534 的 hover:none 清单补上 .hi-clear——那才是根因）。③ 交给 result-presentation 那条用 `class="btn-sm tool"` 白名单的写法，两条不要各做一遍。
- **FIX_SHAPE** 嵌入 DMS 首页双层壳：268px 侧栏 + 品牌顶栏照常渲染 —— 前提全对：integrations/dms-home/index.vue 的 .dms-agent-home 是 `height: calc(100vh - 112px)`；App.vue:2587 是 `<div class="wrap" :class="{ 'has-preview': !!preview }">`；embedded ref 在 :1085；:3919 的抽屉规则可复用。但「样式块加两行」不成立：`.side.open{transform:none}`（:3920）和 `.side-mask{display:block}`（:3921）**都在 @media(max-width:820px) 里**，桌面嵌入态抽屉永远拉不开、遮罩也不出——照原案做完就是把侧栏永久推到屏外。
  - 修正：四条规则不是两条：`.wrap.embedded .side{position:fixed;top:0;left:0;bottom:0;z-index:1150;width:min(300px,86vw);transform:translateX(-105%);transition:transform .18s ease-out}` + `.wrap.embedded .side.open{transform:none;box-shadow:18px 0 50px rgba(17,24,39,.18)}` + `.wrap.embedded .side-mask{display:block;position:fixed;inset:0;z-index:1140;background:var(--scrim)}` + `.wrap.embedded .mobile-menu{display:inline-flex}`。品牌区先不动的判断同意。
- **FIX_SHAPE** metric-card 两份真相源在互相打架 —— 删除那半查实且比提案说的还干净：App.vue 模板里 `.metric-card`/`.mc-*` **一处消费者都没有**（全仓 grep 只命中 3680-3687 的样式行本身），:3679 的 `.kpi-row{display:flex}` 同理；它们唯一的作用就是漏到 ResultPanel 的卡片上，ResultPanel.vue:951 那两个 `text-transform:none;letter-spacing:0` 正是抵消 :3682。所以「深度页那份」这个前提是错的——深度页用的是 .dkpi/.dk-*。后半不可写：.dkpi(:3799)/.dk-*(:3800-3813)/.dh-card(:3815)/.df-card(:3796) 在 App 模板 :3054-3078 真在用，而提议替换成的 `.metric-card` + `.sc-cell` 全在 **ResultPanel 的 `<style scoped>`** 里，App 的元素拿不到 data-v 属性，改完深度页 KPI 是裸 div。
  - 修正：只做删除半刀：删 App.vue:3679-3687 九行（含 .kpi-row）+ ResultPanel.vue:951 的两个抵消声明，净删 ~10 行、零观感变化。.dkpi/.dh-card/.df-card 保持原样——真要统一，得先把 metric-card 那套提到 theme.css 成为全局类，那是另一条提案。
- **READY** 表单控件边框 1.25:1，不过 WCAG 1.4.11 —— 实测复算一致：--border #e2e6ef 对 card/main = 1.25/1.17，暗色 #2c3247 对 #1a1e2b = 1.31；提议 --border-strong #888fa6 = 3.22/3.02，暗色 #636b8c = 3.17，都过 1.4.11 的 3:1。只动表单控件不动卡片/分割线是对的分寸。行号抽查全中（ResultPanel .ask-input:1113、DataMapPanel .dm-path input:1025 / .dm-search:1045、App .vf-select:3436）。小提醒：KbPanel 那六个行号（:3006/:3365 等）指的是多行规则里 `outline:0` 那一行，`border` 在其上一行，按选择器找不按行号找。
- **FIX_SHAPE** 圆角六档并存（token 已就位，只是没人用） —— 计数几乎逐个对上（实测 6px×68、8px×28、5px×26、999px×25、7px×16，var(--radius) 11 处），token 确实闲置。但映射表漏了两档：**4px×11**（.ai-mark:3657、.bubble.user 的 12px 12px 4px 12px 等）与 2px×1，sed 跑完这批仍是字面量，verification 的「只剩 50% 与 0 两类」直接假红。另外这是 140 处、12 文件、user_visible:false 的纯机械改动，视觉回归风险全在「亚像素差」这句自我保证上。
  - 修正：①映射表补 `4px/2px → var(--radius-sm)`（新增一个 token 与「不新增变量」冲突）或明确写「4px/2px 原样保留、断言正则改成 `border-radius: (?!4px|2px|50%|0)[0-9]`」；②别一把 sed 全仓，先只收「同一屏内相邻」的那批（.foundation/.metric-card/.chart-card/.dkpi/.dtable-wrap/.insight-card/.dsec-seg），那是唯一有人看得见的收益，剩下的等下次碰到那个文件时顺手改。
- **READY** NODE_PALETTE 是 GRAPH_PALETTE 的严格子集，重写了一遍 —— 逐色核对属实：DataMapPanel.vue:45 的 7 色全部出现在 panel-utils.ts:41 的 10 色里，是严格子集。:47 的 NODE_FALLBACK_COLOR 与 :86 的 `?? '#8b93ad'` 两处硬编码行号准确，KbMindmap.vue 的 themeColor() 是现成写法。净删一个常量、零依赖。唯一副作用要写进提交信息：取色是 `hash % len`，7→10 会让现有节点换色（不是 bug，是一次性重排）。
- **READY** 移动端 chrome 收口：工具入口进抽屉、快捷条横滑、正文内边距减半 —— 逐处核实：六个工具按钮在 App.vue:2688-2695（加 class tool 后 `.topbar .tool{display:none}` 正好只藏工具、不碰「+ 新会话」和「退出」，比 visual-system 那条的 :not() 链干净）；抽屉里 :2599-2601 的 .sec 是现成模板；.quick 在 :3716 现为 flex-wrap:wrap，:3549 .chat padding 20px 24px、:3551 .bubble padding 12px 16px、:3577 .res-meta 确实没有 flex-wrap；820px 媒体块在 :3917。全部是媒体查询内的样式增补 + 一段抄现成 markup，不新造组件。这条同时吃掉 visual-system「触控热区」那条的 ③，两条别各做一遍。
- **READY** 「本轮理解」提进 foundation summary（零新增高度换一条常显证据） —— 结构核对属实：ResultPanel.vue:584 的 details、:587 的 :open（只在 review/blocked 展开）、:596 的静态标题「问题理解与结果依据」、:602-605 的 foundation-row「本轮理解」四行，全部逐字命中。understandingText 在 :186-188（提案写 189-191，差 3 行，选择器锚定不受影响）。KbAnswer.vue:482 的 .answer-receipt 恒显对照成立。零新增高度这句是真的——summary 那格本来就在渲染。小订正：.foundation-title 在 :902（非 899），窄屏媒体块在 :1132（非 1147）。
- **READY** 混合结果的「AI 综合分析」被渲染两遍 —— 三块位置逐字命中：:3183 `v-if="t.result?.kb && t.result.view?.insight"`、:3189 `v-if="t.page?.insight"`、:3193 已是 `v-else-if`；compoundAnalysis(:2381-2386) 确实先取 `userFacingMarkdown(result.view?.insight)`，与 :3185 同一段文字、同一个「AI 综合分析」标题。3183→3189 之间只隔一个空行和一条注释，Vue 编译器跳过后 v-else-if 能接上。改一个词的 diff，互斥性论证（深度恒有 t.page、混合恒无）成立。
- **READY** 流式回答不跟随滚动：10-20 秒的生成用户看到的是静止画面 —— 全部行号精确：delta 分支在 App.vue:1536-1543 只写 aiTurn.result；scrollDown 定义在 :1974-1976 且用 behavior:'smooth'；五个调用点 1642/1685/1971/2031/2124 一个不多一个不少，全在提问/切会话侧；chatEl ref 在 :1099。「不复用 scrollDown 的 smooth」这个判断是对的——平滑滚动会和每帧新内容打架。8 行 + 120px 阈值 + 120ms 节流，零依赖。
- **READY** 深度页 KPI 环比对毛利率类指标用错单位，同一张卡自相矛盾 —— 两侧口径分叉查实：deep_api.rs:339-340 的 pct 恒按 (current-baseline)/|baseline|*100 算，无 Percent 分支；present.rs:133-142 对 Percent 指标出的是百分点（注释里就写着「按相对算是 +1.7%（实测判官抓获），正确说法是 +0.33pp」）；App.vue:2273 comparisonRate 恒拼 %、:2285-2292 signedComparison 对毛利率出「个百分点」，同一张 .dkpi 卡里并排。isGrossMarginLabel 已在 App.vue:12 导入。诚实地把后端 markdown 那半划给 W5，没有假装一刀全解决。订正：模板调用点在 :3063（`<b>{{ comparisonRate(cmp) }}</b>`），:3062 是 cmp.label；「变化额」那句在 :3067。
- **REJECT** --text-faint 对比度 2.99:1 不过 WCAG AA，被 91 处小字复用 —— 与 visual-system 区那条是同一处改动，而本条给的替换值**过不了它自己写的验收断言**：#6f7791 对 --bg-main 实测 4.18（断言要 ≥4.5，改完当场红）；暗色 #9aa2bd 实测 6.55，比 --text-muted #8b93ad 的 5.44 更亮，三级层次倒挂——本来要修的是「faint 太浅」，改完变成「faint 比 muted 还显眼」。visual-system 那条已经把这两个值算对并写明了不要用这两个数。
  - 修正：token 那半整条删掉，认 visual-system 的 #67708c / #545c76（亮）与 #8a92ad / #a6aec7（暗）。本条只保留字号那半单独成条：ResultPanel.vue:963 .mc-delta-detail 10.5px、App.vue:3862 .dmore 10.5px、DeepTaskPanel.vue:172 .tp-task-acc 10px → 11px（三处行号我都核过，逐字命中），那是独立于颜色的真问题。
- **READY** 「结论与建议」贴着 AI 角标，内容却全是 0-LLM 确定性产物（含权限拒绝裁决） —— 来源溯源查实：present.rs:172 的 doc 逐字写着「确定性 0-LLM」；entity.rs:956 确实把「已按最小权限原则拒绝展示」写进 view.insight，而 ResultPanel.vue:646 的 kicker 就是裸 `AI`。LLM 那两份走 subs 分支被 App.vue:2363 dataOnlyResult 剥掉，链路成立。改动全是模板：:646/:647/:650 三处文案 + `<section v-if="insightCards.length">`（:643-660）整块挪到 :661-682 的 kpi-section 之后、:683 的 sales-context 之前，模板顺序即渲染顺序，无逻辑改动。「数字先于对数字的解读」这条排序也和 .analysis-last 恒在最后自洽。
- **READY** 深度页表格与问数页表格是两套视觉语言，且深度页把已废除的前端行数上限又写了一遍 —— 两套语言查实：ResultPanel.vue:1021 是 `max-width:320px; overflow:hidden; text-overflow:ellipsis`，App.vue:3854 是 `white-space:nowrap`、:3853 是 `min-width:680px`、:3858 首列 sticky 但无行号；App.vue:2261 `const DEEP_TABLE_PREVIEW_ROWS = 24` 与 :3119 的 `.slice(0, DEEP_TABLE_PREVIEW_ROWS)` 逐字命中，确实是第二道前端行数闸。删常量是净删。放开截断的安全性我另核过：sub_ask(deep_api.rs:2180) 走的是 crate::ask，后端 MAX_ROWS 已经限住，且 .dtable-wrap(:3852) 有 max-height:460px + overflow:auto，不会撑爆气泡。
- **READY** 知识库流式期间标题与「直接结论」框随半截 markdown 抖动 —— 逐条对上代码：KbAnswer.vue:348 `const presented = computed(() => presentation(displayMarkdown.value))`，props 里有 `streaming?: boolean`（:25），presentation() 的标题取自 :314 的第一个 heading 且 :316 的排除表是**全词匹配**（'直接结论'），所以半截的「直」「直接」确实会当成自定义标题渲染上去；:322-327 的段落判据用 `/^[-*+]\s+/` 排除列表，半截的「-」不匹配、会被塞进 .answer-summary 蓝框再弹掉。一行 computed 改成三元，html 渲染路径（:350）不动、渐进排版保留。
- **FIX_SHAPE** 单张 KPI 卡在有图表时横跨整行，28px 数字左挂在 800px 空白里 —— 现象真实（:945 是 `repeat(auto-fit, minmax(180px,1fr))`，auto-fit 塌空轨道；soloKpi 在 :157 只覆盖无图无表），但**新规则会打掉窄屏覆写**：`.kpi-row:not(.solo)` 的特异度是 (0,2,0)，而 :1123 的 @container 与 :1136 的 @media 里写的是裸 `.kpi-row`(0,1,0)——媒体/容器查询不加特异度，于是提案里「两处已有覆写保持不变（窄屏仍要 1fr 铺满）」这句不成立，窄屏会继续被 300px 上限卡住。另订正：.kpi-row.solo 在 :966（非 969），@container 在 :1118、@media 在 :1132（非 1141/1156）。
  - 修正：别加 :not() 规则，直接改 :945 本行为 `grid-template-columns: repeat(auto-fit, minmax(180px, 300px)); justify-content: start;`。.kpi-row.solo(:966) 特异度 (0,2,0) 仍压得住 solo 分支，:1123/:1136 的窄屏覆写与 :945 同特异度且在其后，正常胜出——一行改动，两处覆写自动还是有效的。
- **READY** 横向滚动表格 hover 时 sticky 列半透明，底下内容穿帮 —— ResultPanel.vue:1033 `.tbl-wrap tbody tr:hover .row-index { background: var(--primary-light) }` 与 App.vue:3861 `.dtable tr:hover td { background: var(--primary-light) }` 逐字命中；--primary-light 确是 rgba(...,.08)/(...,.14) 半透明；:1030 的斑马行本来就用 color-mix 打的是同一处补丁（同文件已有先例，不是新技术）；App.vue:3858 首列 sticky 属实。两行 color-mix，改的正好是 sticky 那两个选择器、非 sticky 单元格不动。
- **READY** 删两条零消费者的全局样式 —— 实跑 `grep -rn 'scope-note\|tbl-foot' web/src web/tests` 只命中 App.vue:3641 与 :3645 两行样式定义本身，零模板消费者。scope_note 现在确实渲染在 ResultPanel 的 .foundation-body（:625-628 一带的 foundation-row「权限范围」），行数脚注是 .row-count(:1014)。ResultPanel.vue:880-883 那张「本组件依赖的全局类」清单里也确实没有它们。纯删两行，删除 > 新增。
- **REJECT** BiChart 两套调色盘从 JS 常量搬进 theme.css —— 这条什么也不修，还把两件已经工作的事变差。①它与 visual-system 的「单色阶浅端 1.43:1」是同一批常量：那条的验收断言是「读 BiChart.vue 里两条 MONO 数组现算对比度」，搬进 CSS 变量后 node:test 再也读不到色值（vitest/jsdom 不在依赖里，CSS 自定义属性算不出来），等于用一条无用的搬迁废掉一条真判据。②「净删除」不成立：删 4 行 JS 换 28 条 CSS 声明（明暗各 14 个 token）。③现状本来就没坏——:66 的 cssToken、:88-90 的缓存、:380 的 MutationObserver 清缓存已经把主题切换处理干净了，`dark ? DARK : LIGHT` 一个三元不是「复刻一遍暗色逻辑」。
  - 修正：两条调色盘留在 BiChart.vue:61-64。真正要动的只有 visual-system 那条的 MONO 值替换；LIGHT_SERIES/DARK_SERIES 八色我复算过对白卡/暗卡都过 3:1，一个字都不用改。
- **READY** batch_id 落真值：四个写口从调用方拿批次号，空批次号拒绝回滚 —— 本域第一优先级，前提全部查实且比提案描述的更死：exemplar.rs:243-247 / :397-401 / :432-437 三处 log_event 的 batch_id 与 actor 全传字面量 ""（第三处 actor 是 "review"），memory.rs:65-70 传的是 conv_id，而 run.rs:873 给 save_memory 的 conv_id 正是 ""——四个写口 batch_id 恒空，learn.rs:74 的 `AND batch_id <> ''` 让 recent_batches 永远返空列表，账本 100% 查不出来也撤不了。调用点行号逐个核过：run.rs:873(save_memory)、run.rs:905-906(spawn review_failure)、run.rs:927(save_with_context)、review.rs:74/82/101 全中。一处订正：「rollback_batch("") 撤光全部历史」的反面风险已被 admin_api.rs:1921 的 `if batch_id.trim().is_empty() → 400` 挡了一道——但把 ensure! 放进 learn.rs 仍然对，那是唯一所有调用者都会经过的地方。
- **READY** 回滚标记只在真撤成功时落，且不再覆盖 action 列 —— learn.rs:133-136 的 `UPDATE meta.learn_event SET action = 'rolled_back' WHERE id = $1` 确实在 match 之外无条件执行、且 `let _ =` 吞错；:98 的取事件谓词确实是 `action <> 'rolled_back'`，两条一叠就是「撤失败也永久标记，再也重放不了」。改成 rolled_back_at IS NULL + 只在 rows_affected()>0 分支落标记，两条幂等 ALTER 的形态与 ddl.rs:342/368 那批一致。Undone{undone,skipped,failed} 三元返回值也正是 admin_api.rs:1929 现在只能报一个数的原因。一处订正：assessment 里「undone 累加 rows_affected()，0 行也算处理过」不成立——:129 是 `undone += r.rows_affected()`，0 行加 0。要 skipped 计数是对的，但理由得换成「0 行今天完全不留痕」。
- **READY** 回滚加乐观并发守卫：人工复核动过的行一律跳过，不硬撤 —— 这是本域唯一正面回答「学错了怎么撤、撤的时候别把人工成果毁掉」的一条。现状核实：learn.rs:113-117 五条回滚 SQL 全是裸 `WHERE id = $1`，:125 的 before.status 只用来还原不用来比对；exemplar.rs:245 的 after 是 `{"question": ...}`、:399 是 `{"trigger": ...}`，都没存初始 status，所以守卫的前提确实要先补写侧。meta.memory 无状态列、保持无守卫并写明天花板，这个诚实度是对的。一个要点写进实现：exemplar.rs 的 INSERT 若不显式写 status（靠 DDL 默认值），after 里硬编码的 "pending" 会和库里默认值漂——落刀时从 INSERT 的 RETURNING 里带回真值，别猜。注意本条与「回滚标记」「undo_stmt 抽函数」改的是同一段 rollback_batch，必须合成一次提交，不要三刀各改一遍。
- **ALREADY_DONE** 两个 admin 端点（列最近批次 / 回滚一批）——今天账本零调用者 —— 前提是错的：两个端点在工作区里已经写好并挂上了。admin_api.rs:1886-1929 有 `LearnQuery{login_name,role_code,days}`、`pub async fn learn_batches`（含 `admin(&st,&h,...)` 门 + `days.unwrap_or(7).clamp(1,90)` + `recent_batches(pool, days, 200)`）与 `pub async fn learn_rollback`（含 admin 门 + 空 batch_id 400 + `tracing::warn!` 留痕）；main.rs:1373-1374 两条路由 `GET /api/admin/learn` 与 `POST /api/admin/learn/{batch_id}/rollback` 都在。提案里 days 夹紧、admin 门、page_limit、返回体形状，逐条都已存在。
  - 修正：这条整条撤掉。真正还缺的是那条 wire 侧钉板（照 no_create_exemplar_route 用 include_str!("main.rs") 反查两条路由）——把它并进「Undone 三元返回值」那条一起做（端点返回体从 undone 一个数改成三个数时正好动这个文件），别单独立项。
- **READY** AI 初筛与管理员复核两条状态变更补进账本（今天完全在账本之外） —— 两个漏口查实：exemplar.rs:303-326 的 set_ai_review 把语料一把改成 status='disabled' + validation_status='invalid'，函数体内没有任何 log_event；admin_api 侧的四条 EX_* 直写同样绕过 registry。这恰好是「最该能撤的两条」——一个是 LLM 单方面判死、一个是硬删。delete 只留证据不实现回滚、并把 id 换掉这个天花板写进注释，是诚实的做法而不是假装能撤。注意与「set_lesson_status 改 CTE」共用同一个前值取法，两条应同刀落，别写两种形状。
- **READY** set_lesson_status 改成 CTE 一条语句：先落库再记账，顺带省一次往返 —— exemplar.rs:422-449 逐字确认了倒序：先 `SELECT status`（:427-432）→ 再 `log_event`（:433-440）→ 最后才 `UPDATE`（:441-446），而 :447 的 `if affected == 0` 只 warn。也就是说 0 行命中时账本里已经躺了一条从没发生过的变更，将来回滚会拿 before 去「还原」一个没改过的状态——与「乐观并发守卫」那条叠加起来更糟。提议的 `WITH old AS (SELECT ...) UPDATE ... FROM old ... RETURNING old.status` 是 PG 标准前值取法（CTE 读命令快照），一条语句、少一次往返，None 分支保留现有 warn。
- **READY** 把 learn.rs 文件头点名的 learn_writes_are_all_ledgered 真写出来 —— 实跑 `grep -rn learn_writes_are_all_ledgered --include=*.rs` 全仓只命中 learn.rs:12 那句注释本身——测试确实是幻影，文件头白纸黑字承诺的守卫不存在，第五个写口加进来不会有任何东西变红。窗口取法照 drift.rs:82-84 的行窗口是现成的。`assert!(checked >= 5)` 空转跳闸这一手是对的（今天正好 5 处：exemplar.rs 的 sql_exemplar insert / pitfall insert / pitfall status update / sql_exemplar status update，加 memory.rs 的 memory insert）。反向开枪那条验证（注释掉 exemplar.rs:243 必须红）才是这条测试值不值钱的判据，别省。
- **READY** recent_batches 返回时间列：账本要能回答「上周二学了什么」 —— learn.rs:73-79 的 SELECT 确实只出 batch_id/actor/count/tables，min(at) 只在 ORDER BY 里；BatchRow(:61-67) 也没有任何时间字段——「上周二学了什么」结构上答不了，而这正是 learn.rs:69 doc 里自己写的立项理由。`::text` 与 admin_api 现有口径一致、省掉时间类型 feature，符合 D6。零风险附加：admin_api.rs:1904 已经在调它、BatchRow 已 derive Serialize，加字段直接从端点透出去，端点一行不用改。
- **READY** 回滚分支抽成纯函数 undo_stmt：D1 40 行 + 现有钉板恒真两件一起修 —— 两件都核到了：rollback_batch 是 learn.rs:94-139 共 46 行，破 D1 的 40；`rollback_statements_are_compile_time_literals`(:147-157) 只断言每个 LEDGERED_TABLES 名字出现在 match 头里，而 meta.memory 出现在 DELETE 臂（:114）就够了——它没有 update 臂这件事测试确实抓不到，恒真属实。改成打在 undo_stmt 上的真值判据（`undo_stmt("meta.memory", true).is_none()` 并写明这是刻意的、`undo_stmt("meta.term", false).is_none()`）比源码切串强一个量级，且保留一条 `!body.contains("format!")` 兜住拼串。与前面两条同改 rollback_batch，合并成一次提交。
- **READY** 删掉 learn_event.trace_id 列（与 batch_id 恒等的一列白存） —— learn.rs:42 与 :49 确实把同一个 batch_id bind 了两次（$1 和 $8），log_event 的签名里根本没有独立的 trace_id 形参，所以这一列在结构上不可能与 batch_id 不同。ddl.rs:183 的 `trace_id text NOT NULL DEFAULT ''` 位置准确。不写 DROP COLUMN、只从建表语句与 INSERT 里摘掉，现网留一列空串——这个取舍对（DROP 不可逆，为一列不值得）。`sql.matches('$').count() == 7` 这条防漂断言是这次改动唯一会踩的坑（占位符收位时漏一个），钉得准。
- **READY** exemplar.rs 拆出 registry/pitfall.rs：546 行已越过 D2「>500 必拆」 —— `wc -l` = 546，越过 D2 的 450（提案文里写「>500 必拆」与硬纪律的 450 有出入，但结论一样）。要搬的三个函数在 :363-447 一段连续区间，调用点确实只有 review.rs:74/82/101 三处、同一行同参数。不加 re-export 的判断对——留两条路下一个人还会从旧名进。与读侧 recall/pitfall.rs 分层不同、文件头互指，这个提醒是必要的。排序要求：本条改的三个函数正是「乐观并发守卫」「AI 初筛补账本」「CTE」三条要改的对象，先把内容改完再搬文件，或者反过来先搬——总之不要并行开两个分支改同一段。
- **READY** 编译期语义目录补 12 张 ODS 表 + TABLE_PREFIXES 加 scm_，先加门禁再收敛 —— 本批杠杆最大的一条，全链路查实：warehouse_catalog.rs 的 ASSETS 实测 59 条 asset!；recall/schema.rs:54-62 的 catalog_table_filter 把 ASSETS 表名拼成 SQL 白名单，三路召回全过；recall/metric.rs:98-104 是零日志的静默 `.filter(catalog_allows_metric_record)`；t_shop_inspection_records / t_goods_category / t_employee 在目录里 grep 零命中。scm_ 那条杠杆也核实了：registry/mod.rs:353 的 TABLE_PREFIXES 七个前缀无 scm_，而 warehouse_catalog.rs:334 恰好登记了 ywzt_ods.scm_warehous_manage，于是 source_refs 返空 → mod.rs:396 的 `!refs.is_empty()` 恒假 → mod.rs:625 的 catalog_allows_metric 必拒库存指标；同文件 :108-112 的 SOURCE_ASSET_LIVE_PRED 正则确实是同一份前缀的拷贝，提案要求同刀改是对的。「先加门禁（跑一遍种子过闸）再补目录」这个顺序是全批唯一一条不靠人眼盯漂移的做法。
- **READY** 降级护栏把明细型问句打成硬阻断：护栏条件改成「用户要了指标才判聚合」 —— 叠加链逐环查实：intent.rs:1592-1595 的 `if !report.unverifiable.is_empty() && !projections_have_aggregate(...) → conflicts` 就在那里，注释也写着它的本意是「模型多半压根没算用户要的东西」；:1553-1558 未登记筛选名一律进 unverifiable；:1834 的 filter_columns 确实只有状态/商品/客户经销商门店那几族。conflicts 进 blocking()(:1608) 是硬阻断。`intent` 在 coverage_with_evidence(:1457) 的作用域里（同函数已在用 intent.comparisons/requested_detail/regions），`intent.metrics` 字段存在（model 里 :96/:151）——加一个合取项就能写。detail_shape_proved(:1497) 兜形状这个论证也成立。这是把 AX117 两级闸的副作用点名并给出最小修法，不是头疼医头。
- **FIX_SHAPE** 省区映射收敛成一份：CASE 由 shop_business_region_for_province 生成 —— 问题真实且严重：ops_caliber.rs:31 的 province_region CASE 里确实没有上海、没有海南，:72 的 inspection_valid 把 `(province_region(...)) IS NOT NULL` 当有效性过滤，上海门店的巡店记录被整批静默排除；:49 activity_region 的 23 值 IN 列表与 :90 region_of 的省名词表是第三、第四份。但两处不成立：①「shop_business_region_for_province 零消费者」是错的，fastpath/ops.rs:167、:257 与 fastpath/sales.rs:346 三处在用；②「遍历 warehouse_catalog 的省份表」——**没有省份表**，:396 是一个 `match province.trim()` 字面量分支树，没法遍历，而且它返「浙江省区/川渝藏大区」带后缀、province_region 出的是「浙江/川渝藏」不带后缀，还得反向合并 四川|重庆|西藏 这类多对一 regex 臂。
  - 修正：先把权威源变成可遍历的数据：warehouse_catalog.rs 加 `pub const PROVINCE_REGIONS: &[(&str, &str)]`（省份全称/简称 → 带后缀省区），让 :396 的 match 改成在它上面查表（一次改动，两个消费者都受益，测试 :1156-1162 一条不用改）。ops_caliber 侧再从它生成三份形态：province_region 的 CASE 按「同一省区的省份 regex 合并成一臂」生成、activity_region 的 IN 列表与 region_of 的词表按剥后缀去重生成。业务口径（上海→浙江省区、海南→广东省区在运营看板下同样成立）必须先找业务确认，那是数字会变的一刀，不是重构。
- **READY** Doris EXPLAIN 的执行计划别丢：全分区扫描判成可 repair 的缺时间谓词 —— mysql.rs:881 的 `Ok(Ok(_)) => Ok(None)` 逐字命中——计划文本确实付了一次往返然后整条丢弃；:882-885 的两条注释（DB 明确判定才给 Some、超时/抖动给 None）说明 Option<String> 的语义正好适配「计划显示有问题、拿去 repair」。mysql.rs 实测 1641 行，早破 D2，新建纯函数文件而不是往里塞是对的。总数下限 8 这个防误伤旋钮留得好（小表全扫是正常的）。落刀时注意一处 mechanic：现在是 `Ok(Ok(_))` 丢弃行集，取计划文本要改成绑定 rows 并逐行 try_get::<String,_>(0)，提案只写了「拼成文本」没写解码。
- **READY** 深度报告板块问句继承父问已 grounding 的地区/实体（validate_plan 那一刀，不动 ask 链） —— 三处位置查实：validate_plan 在 deep_api.rs:3612（纯函数、已有 2-60 字与 chart 枚举两条判据，加形参不破坏它的可测性）；execute_plan_sections 的 `.flatten()` 在 :2175，失败板块确实静默消失；sub_ask 在 :2180-2196，`crate::ask(...)` 吃的就是裸 `q: &str`。明确划掉「sub_ask 改吃 PreparedAsk」那半（要动 5 个调用点）而先止住错答案，这个分寸对。一条落刀前要确认的：调用侧能否拿到父意图的 Grounded/Resolved surface——deep_api.rs 7462 行，validate_plan 的调用点是否在 PreparedAsk 的作用域内我没核到，如果不在，这条的 effort 从 M 变 L。
- **READY** allowed_dimensions 加一条 CaliberRule，让白名单也管得住 LLM 路 —— 漏面查实：compose/path.rs:146 是 allowed_dimensions 唯一的执行点（`p.allowed_dimensions.iter().any(|d| d == "*" || d == dimension)`），走的是确定性装配门；LLM 路只在 recall/metric.rs:190-193 拿到一句「；允许维度：…」的提示词软约束——白名单在自由 SQL 那条路上确实等于不存在，而那正是未审定组合的落点。CaliberRule 在 kernel/sql/caliber.rs:28 已有 7 个同族变体（RequireCols/RequireCodeOnColumn 等），加第 8 个是这个仓既有的做法，AST 顶层 GROUP BY 提取有现成路径可复用。三条防误伤（空/含 * 不判、分区时间列豁免、进 repair 不 fail closed）一条都不能省，写得对。一处修正：caliber.rs 实测 1840 行、registry/caliber.rs 1338 行，加变体要跨两个都已远超 D2 的文件 + 五条单测，effort 是 L 不是 M。
- **READY** 复合答案：一个子问失败不再整轮 422，且容器补上聚合收据 —— 两处查实：ask.rs:427 是 `let result = one(question.clone()).await?;`——一个子问 Err 整轮上抛，另一个已经查出来的子结果一起丢；ctx.rs:254-283 的 AskResult::compound 里 trust 与 intent_summary 确实写死 None，前端 ResultPanel 的 hasFoundation(:190) 因此整块不渲染，「问题理解与结果依据」在复合结果上永远空白。改 match 收 failed 列表 + 用 compound::missing_note 点名是最短路径。提醒：把 main.rs:2425-2470 的 hybrid_intent_summary 搬进 agent 是**第二件事**（T8 那族的搬运），它和「子问失败不整轮 422」没有依赖关系，分两次提交，别让搬运的回归风险绑架掉那个一眼可验的 422 修复。
- **FIX_SHAPE** 问句切片改用 Query 向量：与整句同空间，且不再挂语料侧熔断槽 —— 核心一行成立：gather.rs:86 确实是 `embed.embed_passages(&slice_texts)`，而同一个 tokio::join! 的第一路是 `embed.embed_query(cx.question)`——整句走 Query、切片走 Passage，同一批召回混两个空间；embed.rs:43-46 的 slot(mode) 确认两种模式共用各自的熔断槽，知识库入库失败会掐 Passage 槽。但「删掉两个包装只留 pub embed_batch」这半有问题：embed.rs:84 **已经有** `pub async fn embed(&self, texts, mode)`，再把 :178 的 embed_batched 提 pub 就是两个公开的批量入口、名字还更像；而且为零用户收益动 5 个调用点。
  - 修正：只改 gather.rs:86 → `embed.embed_queries(&slice_texts)`，同刀把 gather.rs:1041 的断言从 `contains("embed_passages")` 改成 `contains("embed_queries")`（提案写的 `EmbedMode::Query` 在 gather.rs 里根本不出现，那条断言会假红）。两个包装函数保留。verification 里 regression.py 改前后对拍那条必须做——这是全局召回行为的改变，不是纯重构。
- **READY** 语义召回降级写进 trust：口径卡缺席不许还显示 verified —— 这条是本批唯一直击「答错了还很自信」的结构性来源，且证据链完整：gather.rs:317-325 的 BudgetReport 确实带着 `notes: Vec<TrimNote>`；gather.rs:1024-1031 那条既有测试自己就断言「degraded（unwrap_or_default 数）== warns（tracing::warn 数）」且要求 ≥9 路——也就是说这 12 处降级今天只进日志、一个字都不进用户可见的收据。改成 RecallHealth{degraded} 并让 run_llm 在指标/口径类降级时写进 caliber_note、经 ctx.rs 的 risk 判据把 trust 降 review，是顺着既有链路走而不是新造一套。附带删掉恒 false 的 summary_used、恒空的 trimmed 与只有测试生产者的 TrimNote，净删。既有测试扩成三元等式（unwrap_or_default == warn == degraded.push）是防这条将来再漂的正确钉法。
- **READY** LLM 一次重试 + 让 LlmError::Api 这个死变体真正被构造 —— 两件都查实：server/src/llm.rs:311-315 非 2xx 分支把 status 打进日志后 `anyhow::bail!("LLM 请求失败（HTTP {status}）")`，结构化信息当场降级成字符串；kernel/src/llm.rs:64 的 `Api { status, body }` 变体全仓唯一构造点是 :124 的一条单测——确认是死变体。一次重试、800ms、只对 429/5xx 与超时、不做退避框架不做配置，正是这个规模该有的分寸。TcpListener 桩测（第一次 429 第二次 200 → Ok 且只记一次 usage；第一次 400 → 立即 Err 且连接数为 1）是能真正证伪的判据，不是走过场。
- **READY** failure_log 读回来：只有重复出现的失败才配烧一次复盘，且复盘素材不许带权限片段 —— 两条都成立。①`grep -rn failure_log crates/ --include=*.rs` 除 ddl.rs 建表/索引与几条 doc 注释外，全仓零 SELECT——写了不读，一次性抖动和第 7 次重复失败在系统眼里完全一样。②I4 那半更硬：run.rs:901-906 里 `let sql = scoped.wire().to_string();` 之后直接 spawn 进 review_failure，而 scoped.wire() 是**注入行级权限条件之后**的 SQL，它会进 ds 级共享的复盘 prompt 与候选教训——这与 AX118 修 HITL sql-edit 是同一条纪律，值得单独拎出来。新建 registry/failure.rs 而不是塞进已 546 行的 exemplar.rs 也对。一处口误：assessment 说「exemplar.rs:438 是唯一非 DDL 引用」，实际 exemplar.rs 里 grep 不到 failure_log，写口是 exemplar::log_failure_traced——不影响结论。
- **READY** RowSet 带上 truncated：被 ds 策略压到 50 行的结果不再冒充完整结果 —— 契约缺口查实：source.rs:40-46 的 RowSet 只有 columns/rows/redacted，没有 truncated；mysql.rs:842 的 `let (columns, mut data) = to_table(&rows, max);` 那一处正好同时握着 rows 和 max；ctx.rs:312 的 `truncated: row_count >= MAX_ROWS` 逐字命中——ds 策略把 max 压到 50 时这个判据恒假，前端脚注只写「50 行」，既不显示已截断也不给续读。加一个 bool + 两个 to_table 置位 + 一处或运算，且 AskResult.truncated 早在 wire 契约里、前端零改动。这是「静默把部分结果呈现成全量」那一族里最便宜的一刀。
- **FIX_SHAPE** t_employee 从 Global 移到 Scoped（Java EmployeeDao 有 @DataScope） —— 位置对：builtin.rs:119 的 global 循环里确实有 "t_employee"，owner_only helper 在 :24、:67 的 t_invoice_apply_header 是现成用法。前置风险也识别得准且是真的——ops_caliber.rs:72 的 inspection_valid 里那条 `NOT EXISTS (SELECT 1 FROM t_employee oe JOIN t_position op ...)` 排除三方/副总巡店人，转 scoped 后子查询会被注入员工过滤，等于「职位排除只对自己可见的员工生效」＝口径静默放宽、巡店次数虚高。但提案把补救写成「走 via 豁免 **或** 先物化排除名单，两者都要在同一提交里」——两个互斥方案并列，选哪个不定，这就不是能写 diff 的状态；而且 owner_only("employee_id", Ids) 里 employee_id 是不是 t_employee 的自身主键列、Java @DataScope 的实际口径，本仓都无从验证。这是全批 blast radius 最大的一条（改的是谁能看见谁），不该带着未定项落刀。
  - 修正：拆两步。第一步只做不改权限的部分：把 inspection_valid 那条子查询的职位排除名单固化（物化成 `IN (...)` 常量或走 policy 的 via 豁免），并补一条单测断言生成的 SQL 与今天逐字相同——这一步可以独立合入且零行为变化。第二步再动 builtin.rs:119，落刀前必须拿到 Java EmployeeDao 的 @DataScope 注解原文与它实际过滤的列名（不是推断），regression.py 全量跑并把改判题号写进提交信息。
- **READY** Doris 会话钉住时区：CURDATE() 与 PG 侧「今天」同一口径 —— 全批性价比最高的一条。mysql.rs:466-471 那个 after_connect 语句数组逐字命中（今天两条：SET query_timeout = 45 / SET SESSION MAX_EXECUTION_TIME=45000），同一段注释已经写明「失败不阻断建连」，追加第三条 SET 完全落在既有语义里；:457 的 `.timezone(None)` 也确认是 sqlx 握手项、不该动，提案没搞混。分歧前提我另找到一处独立佐证：present.rs:174 的注释「『今天』按业务时区（东八区，与 SQL 侧 CURDATE() 同口径）取：用进程本地时区（容器常 UTC）会在月初/月末当天差一天」——Rust 侧之所以显式钉 +08，正是因为 SQL 侧没钉，跨日边界两套「今天」确实可能不一致。一行、零风险、有源码守卫。