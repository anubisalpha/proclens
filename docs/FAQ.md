# FAQ

Answers here come from things that actually happened while building and testing ProcLens. Where a
cause is a likely explanation rather than something confirmed, the answer says so.

## The Disk & files tab says a built-in check failed, but data is still arriving

The warning reads: *On this PC the built-in check for File activity tracing failed, so the numbers on
this tab may be wrong. See the "This PC" tab for details.* (The name can also be Disk activity tracing,
Driver CPU time tracing or Kernel event tracing.)

**What the check is.** When the collector starts it runs a real kernel trace for about 2 seconds while
it writes and reads back an 8 MiB test file. It then compares what the trace reported with what it
knows it did. The check exists to catch a Windows update or a different Windows build that changed the
undocumented event layouts.

**Why it can fail while data still flows.** The result is only a comparison at one moment: the trace
can be working and still miss the bounds. Trace events arrive in batches about a second late, and the
check waits at most 6 seconds for them. Antivirus scanning the test file can also add reads or writes
that the check does not expect.

**What we saw.** On the development PC the file-activity check failed once, when the collector was
started while a large software build was compiling in the background. After stopping and starting the
collector again with the machine idle, the same check passed. We did not capture the failing detail
line, so heavy load is the likely cause, not a confirmed one.

**What to do.**
1. Close heavy programs, or wait for them to finish.
2. In ProcLens click **Stop collector**, then **Start collector**, and approve the prompt.
3. Open the **This PC** tab. If the check now says it is working, the failure was a one-off.
4. If it fails again, please [open an issue](https://github.com/anubisalpha/proclens/issues) with the
   full line from the **This PC** tab (it states how many MB were written and read, and measured) and
   the output of `collector.exe profile`. That distinguishes a load problem from a real layout change.

## Why does ProcLens ask for administrator approval every time?

Reading kernel-level data needs administrator rights, so the app starts a separate small helper,
`collector.exe`, through the usual Windows prompt each time you click **Start collector**. The window
itself runs as a normal user. The helper only reports data; it cannot end, change or inject into other
programs. See the release notes for details.

## Why does the installer always install to Program Files?

The app starts `collector.exe` from its own folder with administrator rights. If that folder were
writable by ordinary programs, anything running as you could replace `collector.exe` and have it
elevated when you approve the prompt. Program Files is writable only by administrators, so the
installer has no per-user option. If you use the zip, extract it somewhere only administrators can
write for the same reason.

## Windows SmartScreen warns about the installer or the app

The files are not code-signed yet, so SmartScreen shows a warning for the setup file and the programs.
Check the SHA-256 against the value in the release notes before running them.

## Does ProcLens send anything over the internet?

Only if you click **Check for updates** in the footer. That request asks GitHub's public release list
for the newest ProcLens release (betas included) and sends a `ProcLens/<version>` User-Agent; your IP
address is visible to GitHub as with any web request. Nothing is downloaded or installed. Nothing else in
ProcLens contacts the network.

## After uninstalling, an empty `C:\Program Files\ProcLens` folder (and a cache folder) is still there

Seen on the development PC with both the beta.1 and beta.2 installers: the uninstaller reported that it
removed everything, but an empty `C:\Program Files\ProcLens` and the WebView2 cache folder
`%LOCALAPPDATA%\uk.aity.proclens` remained. We did not find the cause. The cache folder is shared with
development builds of the app. Both are safe to delete by hand.

## How do I report a problem on my Windows version?

Run the two diagnostic commands described in the [README](../README.md#checking-your-own-pc-profile-and-discover)
(the Start menu has **ProcLens > Check this PC (discover)** for the second) and attach the output to an
[issue](https://github.com/anubisalpha/proclens/issues). `discover` prints event types and field names
only.
