# 浏览器连接器(桌面侧)

web-next 的「设置 → 连接器 → 浏览器」调本壳的 3 个 Tauri 命令,实现一键启用浏览器
自动化(本地目录装编译版扩展,不经应用商店)。本体跑在本机 edge(tp-agent relay +
Chrome 扩展),桌面壳只做铺扩展 / 装 native host / 起停 relay / 查状态。

## 已实现(本仓,`cargo check` 通过)

- `src-tauri/src/browser.rs` —— 连接器核心:
  - `browser_connector_status` → relay 在线 / 扩展已连(查 `tp-agent browser status --json`
    + `tp-agent browser call browser.relay.health`)+ 已铺扩展目录。
  - `browser_connector_set_enabled(enabled)` → 开:铺扩展(bundle 资源 → `app_data_dir/
    chrome-extension`)+ 装 native host manifest + `tp-agent browser enable` + `tp-agent start`;
    关:`tp-agent browser disable`。
  - `open_chrome_extensions_page` → 打开 chrome://extensions。
- `src-tauri/src/lib.rs` —— 3 命令注册进 `invoke_handler`。

## 待发布管线补齐(否则铺扩展会报「扩展资源缺失」)

1. **打包编译版扩展进 bundle 资源**
   - 构建前 `cd ../../talon-pilot/browser-extension && npm run build`(出 `dist/`)。
   - 把 `browser-extension/{manifest.json,dist,icons,popup.html,popup.css,tokens.css}` 拷到
     `src-tauri/resources/chrome-extension/`。
   - `tauri.conf.json` 加 `bundle.resources`(含 `resources/chrome-extension/`),运行时
     `app.path().resource_dir()/chrome-extension` 即 `browser.rs` 读取的源。
   - 建议在 `release.yml` 做(按 `repos.lock` 的 talon_pilot ref checkout 后构建),不要塞进
     `build.rs` 跑 npm —— 会让本地 `cargo check`/`cargo build` 依赖 sibling 仓 + npm,易碎。

2. **扩展 ID 固定**(native messaging 必需)
   - native host 的 `allowed_origins` 要写死 `chrome-extension://<id>/`,而本地加载的 id 默认
     随机。需在 `talon-pilot/browser-extension/manifest.json` 加固定 `"key"`(公钥)得到稳定 id。
   - 把得到的 id 填进 `browser.rs` 的 `EXTENSION_ID`(当前是占位 `REPLACE_WITH_PINNED_EXTENSION_ID`)。

3. **native host 二进制随 tp-agent 发布**
   - `browser.rs` 把 manifest 的 `path` 指向 tp-agent **同目录**下的 `tp-agent-browser-native-host`。
   - 需让 tp-agent 的发布产物(`agent_download_base` / GitHub Release 那个 tarball)**带上**
     `tp-agent-browser-native-host`(talon-pilot `crates/pilot-agent` 已有该 `[[bin]]`),
     `agent::install_tp_agent` 解包时一并落地。

4. **Windows native host**:走注册表(`HKCU\Software\Google\Chrome\NativeMessagingHosts\...`),
   `browser.rs::native_messaging_hosts_dir` 目前只支持 mac/linux,Windows 待补。

## 验证(需真机 / Tauri 构建,本环境未做)

- `cargo check` 已过;Tauri 构建 + 装 dmg + 设置页开开关 → 铺扩展 → Chrome 加载 → 扩展连上
  relay → 状态变「已连接」的端到端链路需真机验证。
