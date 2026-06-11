# 发布指南（Talon Pilot Studio）

本文是发版的操作手册。发版由打 `v*` tag 触发 `.github/workflows/release.yml`，
自动完成四平台构建、签名、生成 `latest.json`、创建 GitHub Release。**应用内更新弹窗
和 Release 正文的更新说明同源**，都来自 `CHANGELOG.md`。

---

## 一次性准备（仓库 Secrets）

| Secret | 用途 |
|---|---|
| `TALON_PILOT_PAT` | fine-grained PAT，对 `darkmice/talon-pilot` 有 Contents:Read，用来 checkout 私有的 web-next 前端源 |
| `TAURI_SIGNING_PRIVATE_KEY` | `tauri signer generate` 生成的 minisign 私钥（无密码）。公钥已写进 `tauri.conf.json` 的 `plugins.updater.pubkey`，**不可更换**，否则老客户端无法校验新包 |

> 这两个 Secret 缺任意一个，发版流水线会失败（前者 checkout 报错，后者收集不到 `.sig` 会主动 `exit 1`）。

---

## 发版步骤（Checklist）

### 1. 写 CHANGELOG

在 `CHANGELOG.md` 顶部新增一个版本段落，**标题格式必须是 `## <版本号>`**（版本号与 tag 去掉前导 `v` 一致）：

```markdown
## 0.1.7

- 用 Markdown 写：列表 `-`、**加粗**、`代码`、[链接](https://...) 都会在应用内弹窗里渲染
- 链接点击走系统浏览器，不会顶掉弹窗
```

流水线会抽取这一段，**同时**用于 GitHub Release 正文和应用内更新弹窗（`latest.json` 的 `notes`）。
若忘了加对应版本段落，流水线兜底成一句通用说明，不会失败，但弹窗就没有真实内容了。

### 2. 锁定依赖仓的 ref（可复现的关键）

`repos.lock` 决定流水线 checkout 哪个版本的 `talon-pilot`（web-next 前端源）和 `talon-ui`（组件库）：

```toml
talon_pilot_ref = "main"   # 日常可填分支名跟最新
talon_ui_ref    = "main"
```

**正式发版前，把这两项锁成具体 commit SHA**，否则同一个 tag 在不同时间重跑会打出不同前端，
不可复现、出问题也无法回滚到当时的产物。例如：

```toml
talon_pilot_ref = "a1b2c3d4e5f6..."   # 发版当时 talon-pilot 的 commit
talon_ui_ref    = "9f8e7d6c5b4a..."
```

### 3. 源码版本号（可选但建议）

源码 `src-tauri/Cargo.toml` 与 `tauri.conf.json` 的 `version`：

- **发版本身不依赖它** —— 流水线的 `Sync version from tag` 步会在 tag 触发时把 tag 版本（去掉 `v`）
  写进 `tauri.conf.json`，所以发出来的包版本永远等于 tag。
- 但本地/非 tag 构建会用源码里的 version 自报版本。**建议把源码 version 跟最新已发布版本保持一致**，
  否则本地构建会被自动更新反复提示"有新版本"。发完版顺手把源码 version 跟上即可。

### 4. 提交并打 tag

```bash
git add CHANGELOG.md repos.lock src-tauri/Cargo.toml src-tauri/tauri.conf.json
git commit -m "chore(release): 0.1.7"
git push origin main

git tag v0.1.7
git push origin v0.1.7      # 推 tag 触发发版流水线
```

> tag 必须是 `v` 开头（如 `v0.1.7`），否则不触发 `release.yml`。

### 5. 等流水线 + 验证

流水线（约 15–30 分钟，四平台并行）会自动：

1. 按 `repos.lock` 的 ref checkout 三个仓，构建 `talon-ui` → `web-next` → Tauri 打包；
2. macOS ad-hoc 自签、各平台用 `TAURI_SIGNING_PRIVATE_KEY` 产 updater 包及 `.sig`；
3. 从 `CHANGELOG.md` 抽本版本段落 → 生成 `latest.json` 的 `notes` 和 Release 正文；
4. 汇总四平台产物创建 GitHub Release（**非 prerelease**，见下方说明）。

验证：
- Release 页能下载四平台安装包，正文显示本版本 CHANGELOG；
- 装一个**旧版本**客户端，启动后应弹出品牌化更新弹窗、显示本版本说明、可下载升级。

---

## 测试构建（不发 Release）

不想真发版、只想验证能不能构建出来，用 `workflow_dispatch` 手动跑：

- Actions → Release → Run workflow；
- `dry_run` 默认 true（只构建、不创建 Release）；
- 可选 `talon_pilot_ref` / `talon_ui_ref` 临时覆盖 `repos.lock`（留空则用锁定值）。

---

## 自动更新原理（为什么这么设计）

- 客户端启动时拉 `tauri.conf.json` 里 endpoint 的 `latest.json`（GitHub
  `/releases/latest/download/latest.json`），与本机版本比对，有新版才弹更新弹窗。
- 下载包用内置 **minisign 公钥**校验 `.sig`（与 Apple/Windows 代码签名无关），校验失败不会装。
  所以**没有 Apple/Windows 证书也能安全自更新**。
- **Release 不能标 prerelease**：endpoint 用的 `/releases/latest/download/` 只认正式 release、
  跳过 prerelease，标了客户端就永远拉不到 `latest.json`。内测语义放正文文字，不靠 prerelease 标记。
- `pubkey` 一经发布不可更换（换了老客户端校验不过、自更新断链）。私钥务必妥善保管。

---

## 大陆加速（可选，给最终用户/部署方）

直连 GitHub 不稳时，可在客户端配置文件
（macOS/Linux `~/.config/talon-pilot-studio/config.toml`，Windows `%APPDATA%\talon-pilot-studio\config.toml`）
指定镜像，亦可用同名环境变量覆盖：

```toml
update_endpoint     = "https://mirror.example.com/studio/latest.json"   # 自更新 latest.json 镜像
agent_download_base = "https://mirror.example.com/tp-agent"             # tp-agent 安装包镜像(目录内平铺四平台资产)
```

镜像需自行把 Release 里的 `latest.json` + updater 包同步过去；`latest.json` 里的 `url` 仍指向
能下载到对应平台 updater 包的地址。

---

## 回滚

- 产物层面：删除有问题的 Release（或重新发一个更高版本的修复版）。注意 `/latest/` 始终指向
  最新正式 release，删掉坏 Release 后它会回退到上一个。
- 源码层面：因为 `repos.lock` 锁了依赖仓 commit，按当时的 tag + `repos.lock` 重跑流水线即可
  复现当时的产物。

---

## 快速参考

```bash
# 1. 写 CHANGELOG.md 顶部加 "## 0.1.7" 段落
# 2. 锁 repos.lock 的 ref 到 commit SHA
# 3. 同步源码 version(可选)
git commit -am "chore(release): 0.1.7" && git push origin main
git tag v0.1.7 && git push origin v0.1.7      # 触发发版
```
