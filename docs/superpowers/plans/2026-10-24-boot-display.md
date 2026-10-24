# DS916 boot display implementation plan

**Goal:** Display the last saved theme during Windows startup before sign-in, as requested after the successful DeepCool boot test.

**Architecture:** A limited, password-free S4U startup task runs `DS916Tray.exe --boot` under the normal user's account. Reuse the existing theme loader, renderer, sensor poller and COM retry loop. Named global Windows events stop the worker and acknowledge COM release before the interactive tray starts. The tray retains the stop event to block delayed workers.

**Constraints:** Preserve existing uncommitted work, config and themes; no new dependencies; no BIOS changes or automatic reboot. The task loads local profile files; network shares/EFS and live LHM/RTSS data may be unavailable before sign-in. Starting at BIOS/POST is outside Windows application scope.

**Approaches:** Reuse the DeepCool scheduled-worker design (requested); running the tray in Session 0 would create inaccessible UI, and a Windows service adds unnecessary installation and lifecycle complexity.

## Tasks

- [x] Add Windows event handover tests: worker first, tray first, duplicate/late worker, timeout. Run failing tests before implementation.
- [x] Implement `src/boot.rs`; share saved-theme startup in `src/app.rs`; route boot mode separately in `src/main.rs`; separate boot logging in `src/logging.rs`.
- [x] Add `enable-boot-start.ps1`; preserve the task during builds; remove it before uninstalling app data (`build.bat`, `src/tray.rs`). Validate task identity before any mutation.
- [x] Document setup, rollback and limitations in README. Build, run tests, parse scripts and review changes.
- [x] Back up the installed exe, install the new build, register and verify the task. Test Session 0 streaming, sign-in handover and a late task. Restore the tray app; leave an actual reboot to the user.

## Review focus

Boot/interactive overlap must never result in concurrent COM writers. Handover timeout must fail closed. The saved theme must render without LHM. Task removal must fail safely before uninstall deletes data. Builds must stop an existing boot worker before replacing its executable.

## Evidence

Baseline: all seven existing Rust tests pass. The installed binary matches the existing release build. Saved theme is `MyGirl`; COM port auto-detected as COM4. Existing config and theme will be hashed before installation.

Final: all eleven tests pass, including a native regression test that first failed with a restricted thread-token default DACL. Actual Session 0 testing exposed default event permissions denying the normal interactive token (Windows error 5). Events now explicitly allow only the same account and SYSTEM, with a medium integrity label. A focused independent review found no confirmed actionable issues.

The user visually confirmed the saved theme while only the Session 0 worker was running. After the permissions fix, actual boot-worker handover exited with task result 0; the tray reopened COM4 and sent its first frame. A delayed task skipped correctly. An isolated preview rendered the real saved 462x1920 theme without LHM. Config and theme hashes stayed unchanged. Task definition assertions verified S4U, limited rights, boot trigger, --boot action, unlimited runtime and duplicate prevention. Boot startup is enabled. The original installed exe is backed up under target/boot-start-backup/.

An unattended real reboot and destructive uninstall were not performed. Sensor providers were not reconfigured. The same event-permissions defect was also confirmed in the earlier DeepCool worker's live log, corrected and installed in that project. Its actual Session 0 handover then passed with task result 0. Both normal tray apps were restored, and both boot tasks remain enabled.
