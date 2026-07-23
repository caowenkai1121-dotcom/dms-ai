# 三端接入指引

DMS 智能助手三端形态：①独立 Web ②DMS 首页嵌入 ③企业微信。三端共用一套后端与认证。

## 认证总览

| 端 | 认证方式 | 身份来源 |
|---|---|---|
| ①独立 Web | 开发/内网：body 传 login_name（临时）；生产可接 DMS 登录转发 | login_name |
| ②DMS 嵌入 | **SSO 换签**：iframe URL 带 DMS token → 验真 → 会话 token | DMS getLoginInfo |
| ③企业微信 | 企微 OAuth（M5 续）→ userid 映射员工 → 会话 token | 企微 userid |

权限计算三端一致：principal 从 DMS 生产库只读现算（1:1 复刻 @DataScope），不依赖 DMS 接口。

## 端#2 DMS 首页嵌入（零 DMS 源码改动）

利用 DMS 现成的**外链菜单 iframe 机制**（`frameFlag=1, frameUrl=...`）：

1. **DMS 后台配置外链菜单**（系统管理 → 菜单管理，你在 DMS 后台点配置，非改代码）：
   - 菜单类型：菜单，勾选"外链/iframe"
   - 外链地址：`http://<助手host>/?dms_token={当前登录token}&role={当前激活角色}`
   - 替换首页：把"首页"菜单指向此外链即可
2. **DMS 前端透传 token**：`frameUrl` 里的 `{token}` 由 DMS 前端用 `localStorage['smart_admin_user_token']`（DMS token 键）拼接。
   - 若 DMS 不支持 URL 模板变量，可让 DMS 侧在菜单渲染时拼 `?dms_token=` + 当前 token（一处小改，或用 postMessage 传）。
3. **助手前端 boot**（已实现，`web/src/App.vue`）：
   - 检测 URL `dms_token` → `POST /api/sso {dms_token, role_code}` → 验真 DMS `getLoginInfo` → 颁会话 token
   - 后续 `/api/ask` 带 `Authorization: Bearer <会话token>`，免登、自适应、身份即 DMS 当前用户。

## 认证接口

- `POST /api/sso` `{dms_token, role_code?}` → `{token, login_name}`：验真 DMS token 换会话 token（12h 闲置 TTL，活跃滑动续期）。
- `POST /api/ask` header `Authorization: Bearer <token>` + `{question}`：会话 token 优先；无 token 回退 body.login_name（开发）。

## 已验证（2026-07-23）
- SSO 对接生产 DMS getLoginInfo：假 token 正确返回验真失败（code 30007）。
- 前端嵌入 boot：URL dms_token → 隐藏登录框 → 自动 SSO → 失败提示准确。
- 真 token 成功路径（DMS 登录需图形验证码，无法自动化）：代码逻辑对称，getLoginInfo 返回 data.loginName（LoginService.java:464 getLoginResult）；生产嵌入时由 DMS 前端透传真 token 即可。

## 待做（M5 续）
- 企微 OAuth：`/api/wework/callback?code=` → 企微 userid → t_employee 映射（手机号/loginName 对照表）→ 会话 token。企微配置：corpid `wwd8304eb63d2cb14c`（secret 在凭证档，不入库）。
- 端#1 独立登录：SM4 加密（密钥 `1024lab__1024lab`，DMS 前端同款）转发 DMS `/login`（含验证码流程）。
