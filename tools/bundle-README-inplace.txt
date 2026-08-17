dms-ai 部署包 —— 就地升级（针对 1.95.7.181 现网布局）
================================================================

【怎么装】三条命令，在服务器上跑

    tar -xzf dms-ai-部署包-*.tar.gz -C /root
    cd /root/dms-ai-部署包-*
    bash 安装.sh

想先看它会做什么、不实际改动：  bash 安装.sh --dry-run


【它会做 8 件事，每步幂等，重复跑收敛】

  0  前置检查        root / docker / python3 / /opt/dms-ai 在不在
  1  备份            会被替换的目录另存到 /opt/dms-ai/rollback-before-<时间戳>
  2  同步源码        settings.docker.json / .secret_key / kbdata / venv / web/dist 一律不碰
  3  装配置          缺了才装，已有的绝不覆盖
  4  向量服务转容器  ★ 替掉 nohup 起的裸进程（见下）
  5  构建后端 + 重启 沿用现网的 scripts/server-build.sh + server-restart.sh
  6  换前端产物      包里带的是构建好的 dist，服务器不需要装 Node
  7  导入业务字典    ★ 幂等（见下）
  8  上线验收        逐表对账 + 服务托管形态


【★ 为什么要把向量服务改成容器】

旧版说明书第 3 步教的是：

    nohup venv/bin/python tools/embed_service.py serve 8078 172.17.0.1 > embed.out.log 2>&1 &

照做没错，但这样起来的进程有三个问题，且**在 /api/health 上全是绿的**：

  · 机器一重启，服务就没了（没有任何东西负责把它拉起来）
  · 部署换了代码它不跟着变 —— 进程还抱着旧文件，表现是「改了等于没改」
  · 谁都不知道它在跑，systemctl 查不到它

现在改成容器 dms-ai-embed：依赖（LibreOffice、tesseract、Python 包）和代码都在镜像里，
带 --restart unless-stopped，机器重启自己回来。安装脚本会自动接管 8078（中断数秒）。

单独重装/升级向量服务：

    DMS_RUNTIME_ROOT=/opt/dms-ai DMS_EMBED_TAKEOVER=1 bash /opt/dms-ai/scripts/embed-install.sh


【★ 为什么必须导入业务字典种子】

registry_snapshot.json 里是**人工沉淀**的东西：SQL 样例、维度同义词、码值字典、教训。
代码种子里没有这些。2026-08-17 实测：这台机器少了 90 条 SQL 样例、48 条教训，
结果「本月销售额按省份的分布」这类问句直接答「不可计算」，而老服务器答得出来。

导入是幂等 upsert，重复跑安全。


【装完怎么确认真成了】

    DMS_RUNTIME_ROOT=/opt/dms-ai bash /opt/dms-ai/scripts/server-verify.sh

它核对 /api/health 答不了的四件事：
  · 注册表逐表行数（基准取自包里那份快照自己）
  · SQL 样例的向量覆盖率（health 的 vector_ready 只覆盖三张表，样例表不在里面）
  · 向量服务是不是真被托管（端口有响应 ≠ 有人管它）
  · 版本布局

安装脚本第 8 步已经自动跑过一次，这条是给以后随时复查用的。


【人工冒烟三题】

  1. 本月销售额                  → 应走确定性问数，带口径收据
  2. 销售额按省份的分布           → 应出 20+ 行省份数据（这题此前答「不可计算」）
  3. 市场费用的报销政策是什么      → 应走知识库，带引用


【出问题】

  现象                              多半是
  ------------------------------    ----------------------------------------
  停在「后端镜像构建失败」            服务器拉 crates.io 慢；/root/.cargo/config.toml
                                    放 rsproxy 镜像后重跑
  停在「解析服务无法读取知识库探针」    向量服务没起来：docker logs dms-ai-embed --tail 50
  停在「HEALTH TIMEOUT」             后端起了但健康检查不过：
                                    docker logs dms-ai-server --tail 50
  「发现上次部署遗留容器 …-rollback」  上次中断留下的，核对后
                                    docker rm -f dms-ai-server-rollback 再重跑
  验收报 meta.xxx 行数不足           快照没导全，重跑 bash 安装.sh 即可（幂等）

后端重启失败会自动恢复旧容器，前端更新失败会自动恢复旧目录 —— 生产不会停在半路。
真要整体回退：从第 1 步那个 rollback-before-<时间戳> 目录拷回来，再跑一次
scripts/server-build.sh + server-restart.sh。


【包里有什么】

  安装.sh                           上面那个安装器
  部署说明.txt                      本文件
  source/                           完整源码
  payload/web-dist.tar.gz           前端构建产物
  payload/registry_snapshot.json    业务字典种子
  payload/requirements-embed.lock.txt  现网 Python 依赖快照
  config/settings.docker.json       生产配置（数据库连接 + 大模型 key，加密态）
  config/secret.key                 上面那份配置的解密密钥
  MANIFEST.json                     版本与每个文件的 sha256

config/ 两个文件合起来等价于明文凭据，当密码本对待：别进 git、别外传、别放公共盘。
安装脚本只在服务器上**没有**配置时才装入，不会覆盖现网那份。
