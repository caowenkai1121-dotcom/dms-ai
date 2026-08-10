# 三端接入指引

DMS Agent 提供独立 Web、DMS 首页嵌入和企业微信三种入口。三端共用同一套身份、角色、数据权限、语义口径和查询闸门。

## 认证总览

| 入口 | 登录方式 | 身份来源 |
|---|---|---|
| 独立 Web | DMS 账号和密码，无验证码 | DMS 登录接口 |
| DMS 首页 | 当前 DMS token 经 `postMessage` 换签 | DMS `getLoginInfo` |
| 企业微信 | OAuth code 换 userid，再映射 DMS 员工 | 企业微信通讯录 + DMS 员工 |

所有业务请求最终加载同一个 DMS principal。账号状态、角色归属和数据范围在服务端重新校验；多角色未明确选择时拒绝执行业务查询，不合并权限。

## 独立 Web

用户只输入 DMS 账号和密码。Agent 服务端转发 DMS 登录并签发自己的短会话，浏览器不保存密码。独立入口不要求图形验证码，也不接受仅提交 `login_name` 的开发旁路。

## DMS 首页嵌入

DMS 首页必须使用仓库中的 [`integrations/dms-home/index.vue`](../integrations/dms-home/index.vue) 接入，不能把 DMS token 拼进 iframe URL。

流程：

1. DMS 首页立即加载 `${VITE_AGENT_DOMAIN}/?embed=dms-home`。
2. 子页通过精确父页 origin 发送 `dms-ai:ready`。
3. 父页读取 `smart_admin_user_token`，只向精确 Agent origin 发送 `dms-ai:sso`。
4. 子页调用 `POST /api/sso`；服务端向 DMS `getLoginInfo` 验真，再签发 Agent 会话。
5. 子页返回 `dms-ai:sso-ok` 后移除加载遮罩。会话过期时重复握手，无第二次登录。

安全约束：

- token 不进入 URL、浏览历史、Referer、日志或静态文件。
- 父子页面同时校验 `event.source` 和精确 `event.origin`，不使用 `*`。
- iframe 只负责 UI；权限仍由后端 principal 和 SQL 闸门执行。
- 本项目不直接修改只读 DMS 源码；应用步骤与回滚方式见 [`integrations/dms-home/README.md`](../integrations/dms-home/README.md)。

## 企业微信

企业微信自建应用使用 OAuth 网页授权：

1. 企业微信后台配置可信域名、可信 IP 和应用可见范围；运行时设置 `wework_redirect_url` 必须与后台回调地址完全一致。
2. 应用入口指向 `GET /api/wework/start`。服务端生成 5 分钟、一次性的 OAuth state，并用 HttpOnly Cookie 绑定发起浏览器。
3. 回调同时校验 query state、Cookie 和服务端票据，校验即消费；不接受直接拼接的 `/api/wework/login?code=...`。
4. `code` 换取 userid，再读取企业微信用户资料。
5. 服务端按已配置的稳定标识映射 DMS 员工，并重新校验账号状态和角色。
6. 映射成功后签发与另外两端相同的 Agent 会话；无法唯一映射时拒绝登录。

企业 ID、应用 secret、agentid 等仅存放在被 gitignore 的运行时设置文件中，不写入源码、文档、日志或镜像层。

## 关键接口

- `POST /api/login`：独立 Web 的 DMS 账号密码登录。
- `POST /api/sso`：DMS token 换 Agent 会话。
- `POST /api/session/role`：显式切换当前角色并重新校验归属。
- `GET /api/wework/start`：企业微信应用入口，生成 state 后跳转授权页。
- `GET /api/wework/login`：仅供企业微信携带合法 code/state 回调。
- `POST /api/ask`：携带 `Authorization: Bearer <session token>` 的统一问数入口。
