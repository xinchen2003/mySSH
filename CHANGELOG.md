# Changelog

自 v0.3.x 起按版本维护。格式分组：新功能 / 修复 / 变更。

## [0.3.4] - 2026-09-18

### 新功能

- 自动更新：启动时静默检查 GitHub Releases，发现新版本弹确认后下载安装并重启（轴一 1.2）
- 一键导出诊断包：设置 → 常规 → 诊断，日志 + 系统信息打包 zip

### 修复

- 连接建立期（term_open 未返回）键入丢失：输入订阅提前注册，开链前键入入队缓冲、开链后按序补发
- 终端搜索条与 pane 悬浮关闭按钮错位重合：搜索条右移让位
- trzsz 此前从未真正可用：zmodem.js 哨兵透传吐裸 number[]，trzsz 检测器静默跳过（类型归一化修复）；触发串识别无跨块缓冲，WAN 切包后传输不启动（`createTrzszTriggerGuard` 行尾暂存，全切点测试锁定）——真机验收双向哈希一致

### 变更

- 会话意外断开路径补结构化日志（core-ssh 区分 EOF / Close / 传输层终止；supervise 记录重连判定与耗尽）——为「会话中期掉线」定位提供证据链
- 多行粘贴确认判定抽为纯函数 `term/paste.ts` 并补边界测试

## [0.3.3] - 2026-09-17

### 新功能

- **主题扩展为整体换肤**：界面配色从主题 xterm 调色板自动派生；8 个内置主题与自定义主题同一逻辑
- **设置弹窗改版**：720px 五页签左导航；新增黑夜/GitHub/护眼绿/暖阳 4 主题；ThemeStudio 图形化自定义主题编辑器
- **MCP 交互式终端工具**：`terminal_open` / `terminal_send` / `terminal_read` / `terminal_close`（14 → 18 工具）
- **审计保留策略**：启动时自动清理 90 天前的审计记录

### 修复

- 子菜单改为主菜单同级渲染——毛玻璃 `backdrop-filter` 形成包含块导致嵌套 fixed 脱离视口
- CI 双红：`.gitattributes` 固定 LF；rust job 补前端构建前置

### 变更

- 下线 AI 审计面板与 ssh_config 批量导入（产品决策；audit 表与写入点保留）

## [0.3.2] - 2026-09-16

### 新功能

- **ZMODEM（rz/sz）与 trzsz 终端内文件传输**
- **登录宏**：认证进 shell 后逐行自动执行
- **MCP**：SFTP 工具族；工具分组权限配置 + 会话级权限覆盖；`sftp_upload` / `sftp_download` 传输工具（后台队列，无上限）
- **SFTP 跟随终端**：目录上报总开关，服务器侧 OSC 7 集成（脚本本体 `~/.myssh/osc7.sh`）
- 会话编辑器改版：认证并入基本信息页签；隧道列表两行卡片；隧道绑定固定归属会话

### 修复

- ZMODEM 收方向会话缺 `start()` 致 sz 下载永久死等

## [0.3.1] - 2026-09

### 新功能

- **su 二级登录**：登入后自动 `su - <目标用户>`；密码存 DPAPI 保险库，密码提示自动应答一次（答错不重试防锁定）；断线重连后自动重放

### 修复

- 测试连接：首次连接未知主机不再必然失败（AcceptOnce 放行不学习；密钥变更仍拒绝）
- 粘贴：修复 Ctrl+Shift+V 粘贴两次（Chromium 原生 paste 双发）；新增 Ctrl+V 粘贴
- 确认弹窗：Enter = 确认（全部确认框）；2FA 弹窗输入框 Enter = 提交
- 安全：移除登录后 shell 注入（stty/OSC 7 钩子、分屏 cd）；OSC 7 改纯被动解析

[Unreleased]: https://github.com/xinchen2003/mySSH/compare/v0.3.4...HEAD
[0.3.4]: https://github.com/xinchen2003/mySSH/releases/tag/v0.3.4
[0.3.3]: https://github.com/xinchen2003/mySSH/releases/tag/v0.3.3
[0.3.2]: https://github.com/xinchen2003/mySSH/releases/tag/v0.3.2
[0.3.1]: https://github.com/xinchen2003/mySSH/releases/tag/v0.3.1
