# Zerona

Control your coding agents (Claude Code, Codex, Cursor, Devin, Grok, Hermes, Pi, Antigravity) locally by default, with optional multi-device sync.

This repository is a **fork** of [zeronsh/zeron](https://github.com/zeronsh/zeron) maintained by pancaprima. It keeps upstream sync and auth contracts (`edge.zeron.sh`, `zeron://`, `ZERON_*` env vars) so you can still use the shared backend while shipping fork-specific features:

- **Content search** across workspace files
- **Persistent browser profiles** for harness sessions

*English | [简体中文](README.zh-CN.md) | [한국어](README.ko.md) | [日本語](README.ja.md)*

![Zerona desktop app](docs/media/readme/app-screenshot.jpg)

## Desktop app

Download the latest release for your platform from [GitHub Releases](https://github.com/pancaprima/zerona/releases/latest):

- **macOS** — `zeron-<version>-macos-arm64.dmg`
- **Windows** — `zeron-<version>-windows-x86_64-setup.exe`
- **Linux** — `zeron-<version>-linux-<arch>.tar.gz`, then run its `install.sh`

No account or network connection is needed; sessions stay on your device. The app updates itself.

## Headless (CLI)

The CLI binary is still named **`zeron`** (same as upstream installers and release artifacts); **Zerona** is the product name shown in the desktop app and docs.

For servers and other machines without a display, such as a VPS that keeps agents running after you close your laptop. Linux only:

```bash
curl -fsSL https://zeron.sh/install.sh | sh
zeron status
```

The installer starts the engine as a background service that survives reboots.

```bash
zeron status      # local/synced mode and engine status
zeron update      # update to the latest release
zeron daemon start|stop|restart|status
```

## Multi-device sync (optional)

Sign in to start an agent on one device and follow or drive it from another:

```bash
zeron daemon stop
zeron login        # or: zeron logout to return to local-only
zeron daemon start
```

Devices signed in to the same account can read and write each other's workspace files, so only sign in devices you trust. Existing local sessions are never uploaded.

## Forked from zeronsh/zeron

Zerona is derived from [zeronsh/zeron](https://github.com/zeronsh/zeron) under the [MIT License](LICENSE). Copyright (c) 2026 Wing. The upstream project and its contributors retain copyright on the original work; see the upstream repository for their license and attribution.

---

Developing or curious how it works? [Ask DeepWiki](https://deepwiki.com/zeronsh/zeron) or check out [ARCHITECTURE.md](ARCHITECTURE.md).

Licensed under the [MIT License](LICENSE).
