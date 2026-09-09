# dsh-work-upgrade

DeepSeek Work 桌面应用的版本检查与升级插件（dsh 插件，随 DeepSeek Work 预装）。

在「设置 → 通用设置」中展示当前应用版本，支持检查更新与一键下载升级包：

- **国内源优先**：版本号解析自官网 `www.hotpotliuyu.com/ds-work/js/main.js`，升级包下载自官网静态目录（zip）
- **GitHub 兜底**：国内源不可达时回退 GitHub Releases（dmg / x64-setup.exe）
- 下载完成后自动打开安装包（macOS 挂载/解压，Windows 拉起安装器或定位文件），按提示完成安装

仅桌面端（Tauri 壳）可用；纯浏览器环境显示「仅桌面端可用」。

## 开发

```bash
node scripts/build.mjs   # 产出 lib/index.js（host）+ lib/client.js（浏览器）
pnpm run typecheck       # 需先 pnpm install
```
