---
name: release
description: "Build and release InstaLock to PC. Use when user says 'release', 'compila', 'build', 'deploy a PC', or wants to compile the Tauri app. Handles version bump, git push, the build (locally or on the remote PC via SSH), and copying the MSI to Downloads."
---

# Release InstaLock

Build the InstaLock Tauri app and put the MSI installer in `C:\Users\rocon\Downloads\`.

The build target is the Windows PC `PC_ARNAU`, where the repo lives at
`C:\Users\rocon\dev\instalock`. **The session may already be running on that
machine**, in which case there is nothing to SSH into.

## Step 0 — decide local or remote

Run `hostname`. If it prints `PC_ARNAU`, you are on the target machine: **skip
steps 2 and 3 entirely** and run the build locally. Otherwise use the SSH path.

Never run the SSH commands blindly — `ssh pc` only resolves from machines that
have a `pc` host in `~/.ssh/config`, and there is no such entry on `PC_ARNAU`.

## Steps

1. **Bump the version** — `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`
   and `package.json` must all match, and the previous version has usually
   already shipped. Then run `cargo check --manifest-path src-tauri/Cargo.toml`
   so `Cargo.lock` picks up the new version.

2. **Commit pending changes** if any (don't ask, just commit with a descriptive
   message). Stage with `git add -u` so untracked junk is never swept in.

3. **Push**: `git push`. Retry a few times if it fails. The remote is
   `https://github.com/Roconx/instalock.git`.

### Remote path only (skip when `hostname` is `PC_ARNAU`)

4. **Pull on PC**: `ssh pc "cd ~/dev/instalock && git pull"`
   - If the pull is blocked by local changes, **stop and report it**. Do not run
     `git stash`/`git checkout -- .`/`git clean -fd` unprompted: on the target
     machine that is the user's own working tree and those commands throw away
     uncommitted work. Ask first.

5. **Build on PC**: `ssh pc "cd ~/dev/instalock && npx tauri build 2>&1"`
   (timeout 600s)

6. **Copy MSI**: `ssh pc "Copy-Item '<msi-path>' 'C:\Users\rocon\Downloads\'"`

### Local path

4. **Build**: `npx tauri build` from the repo root (timeout 600s).

5. **Copy MSI**: `Copy-Item '<msi-path>' 'C:\Users\rocon\Downloads\' -Force`

6. **Installing** requires closing the running app first and elevation, so it
   often hits a permission prompt. If it does, don't work around it — hand the
   user the MSI path and the command and let them run it.

MSI path (both paths):
`C:\Users\rocon\dev\instalock\target\release\bundle\msi\InstaLock_<version>_x64_en-US.msi`
The filename version comes from `src-tauri/tauri.conf.json`, so a version that
drifted between the three files shows up here as a "file not found" on the copy.

## Notes

- The PC's shell is PowerShell — use `Copy-Item`, not `cp`.
- Don't use `grep`/`find` over SSH on the PC; they aren't PowerShell commands.
- A remote build script has previously copied files at the wrong depth, creating
  `src/src/`, `src-tauri/src/src/` and `src-tauri/src-tauri/`. They're gitignored
  now; if they reappear, the copy step is writing one level too deep.
