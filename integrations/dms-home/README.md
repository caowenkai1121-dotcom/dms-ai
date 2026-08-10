# DMS 首页嵌入组件

本目录提供 DMS 前端首页的单文件替换件。AI 项目不会直接修改 `xh-dms-fornt` 只读源码。

## 接入

1. 在 DMS 前端环境文件中设置 `VITE_AGENT_DOMAIN`，值为可从浏览器访问的 Agent Web 根地址。
2. 由 DMS 前端维护方备份并将本目录的 `index.vue` 应用到：
   `src/views/system/home/index.vue`
3. 按 DMS 原有发布流程构建和发布。

本地联调可配置：

```dotenv
VITE_AGENT_DOMAIN=http://localhost:5180
```

## 认证流程

1. iframe 立即加载 `?embed=dms-home`，不携带用户凭据。
2. Agent 子页发送 `dms-ai:ready`。
3. DMS 父页从 `smart_admin_user_token` 读取当前 token，并仅向配置的 Agent origin 发送 `dms-ai:sso`。
4. Agent 后端向 DMS 验真并签发自己的短会话；成功后子页发送 `dms-ai:sso-ok`。
5. Agent 会话过期时再次请求父页发送当前 DMS token，无需第二次登录。

token 不进入 URL、浏览历史、访问日志或构建产物。多角色用户由 Agent 内部展示角色选择，父页不猜测或合并权限。

## 回滚

恢复 DMS 前端原 `src/views/system/home/index.vue` 后重新构建即可；后端、菜单和业务库均无需变更。
