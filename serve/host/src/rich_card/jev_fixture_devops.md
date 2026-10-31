# Machine health, 2026-11-01 00:25

**Overall: OK, but the disk is trending down and swap is in use.**

| | Now | Note |
|---|---|---|
| **Disk (internal)** | 22.7 GB free (10%) of 228 GB | It was 65.7 GB at 12:17 yesterday after the reboot. It has lost about 43 GB since then (45.0 at 16:17, 28.6 at 20:17, 22.7 now). |
| **Disk (external)** | Macos 1TB 450 GB free, RunnerWork 345 GB, OrcaWorkspaces 184 GB | Fine. |
| **Memory** | 16 GB total: apps 7.2, wired 2.0, compressed 2.8, free 1.0 | `free_pct` is 68%, so there's no pressure right now. |
| **Swap** | 2.9 of 4.0 GB used | Swap is over-committed. It was about 8 GB last week, so it's better, but it's still sitting at 73% of its cap. |
| **CPU** | Load 9.9, 9.0, 5.8 on 10 cores | `rustc` is at 94%, Claude sessions at 50%, ScreensharingAgent 44%, Tailscale network extension 42%. |

**Memory by app:**
- Claude: 3.8 GB across 11 processes.
- `rustc`: 1.3 GB.
- Hermes: 0.8 GB across 34 processes.
- Orca: 0.7 GB.
- 24 terminal sessions are open.

**Devops monitor, last 60 samples:**
- Load peaked at 13.3.
- `rustc` was the top CPU user in 15 samples.
- Internet latency averaged 17 ms with no offline time.
- Two problems are open: swap (since 18:11) and bridge (since 10:26).

**What needs you:**
1. **Disk is falling about 15–20 GB per 4 hours during Kith builds.** Cargo and Xcode builds are the likely cause, but I haven't isolated which one. The last daily run (00:15) listed these cleanable caches:
   - DerivedData, 51.2 GB. It's on the external disk, so it doesn't help internal free space.
   - uv, 5.3 GB.
   - pnpm, 2.2 GB.

   The uv and pnpm caches regenerate, and I can clean them if you say yes. They only gain a few GB, though. The real fix is finding which build target dir is writing to internal disk. I'd measure that next.
2. **Bridge alert open since 10:26.** That's devops's lane. I haven't checked what it is.
3. **Kernel panic and reboot at 12:09 yesterday (watchdog timeout).** I can't say whether it's related to the current load. If it happens again, it's worth passing to devops.

I haven't changed anything. Say "clean uv and pnpm", or "find the disk grower", and I'll do that next.