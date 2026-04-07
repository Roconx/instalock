---
name: release
description: "Build and release InstaLock to PC. Use when user says 'release', 'compila', 'build', 'deploy a PC', or wants to compile the Tauri app on the remote PC via SSH. Handles git push, SSH build, and copying the MSI to Downloads."
---

# Release InstaLock

Build the InstaLock Tauri app on the remote Windows PC via SSH and copy the MSI installer to Downloads.

## Steps

1. **Commit pending changes** if any (don't ask, just commit with a descriptive message)
2. **Push to GitHub**: `git push` (remote is `git@github.com:Roconx/instalock.git`). Retry a few times if fails.
3. **Pull on PC**: `ssh pc "cd ~/dev/instalock && git pull"`
   - If local changes conflict: `ssh pc "cd ~/dev/instalock && git stash && git checkout -- . && git clean -fd && git pull"`
4. **Build on PC**: `ssh pc "cd ~/dev/instalock && npx tauri build 2>&1"` (timeout 600s)
5. **Copy MSI to Downloads**: `ssh pc "Copy-Item '<msi-path>' 'C:\Users\rocon\Downloads\'"`
   - MSI path: `C:\Users\rocon\dev\instalock\target\release\bundle\msi\InstaLock_<version>_x64_en-US.msi`
   - Read version from `src-tauri/tauri.conf.json`

## Notes
- PC shell is PowerShell — use Copy-Item, not cp
- Never use grep/find on PC
- Version bump: update `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `package.json`
