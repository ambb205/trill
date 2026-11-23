# Installing and updating

Trill runs on Windows, on Macs with Apple silicon and on Linux on x86-64. A release is two programs and their
licences: `trill.exe` (on a Mac `Trill.app`, on Linux `trill`), which is Trill, and `trill-mcp.exe` (on a Mac
`Trill.app/Contents/MacOS/trill-mcp`, on Linux `trill-mcp`), the same program without the window for an AI client
that speaks MCP. Nothing else is installed with them, and nothing
they need has to be installed first.

## Installing

Releases are on [GitHub](https://github.com/ambb205/trill/releases). Each comes as two files, and either gives
you the same Trill:

- **`Trill-<version>-setup.exe`**, the installer. It installs for you alone, into your own folder
  (`%LOCALAPPDATA%\Programs\Trill`), and asks for no administrator's word. It adds Trill to the Start menu and, if you
  leave the box ticked, opens songs (`.trill`) and making-ofs (`.trillstory`) with Trill when you double-click them.
- **`trill-<version>-windows-x86_64.zip`**, the same files as an archive. Unpack it anywhere you can write and start
  `trill.exe`. Nothing is added to the Start menu and no file opens with it by itself.

Trill is not signed with a certificate yet, so Windows may say that it does not know the program the first time
you run the installer: "More info", then "Run anyway". `SHA256SUMS.txt` beside each release has the checksum of
every file of it.

The installer waits while a Trill is running, and closes none: a song that is open is yours to save first.

**What Trill keeps, and where.** Its settings are in `%APPDATA%\trill`. What it fetches when you ask it to (the
models of studies, sampled instruments) and your studies are in `%LOCALAPPDATA%\trill`, unless you chose another
place in File > Setup. Your songs are wherever you saved them. Trill changes nothing else on the computer.

**Taking Trill off again.** "Installed apps" in Windows' settings has Trill; a copy from the archive is removed by
deleting its folder. Either way your songs, your settings and what was fetched stay where they are: delete those
two folders yourself if you want them gone.

## On a Mac

Each release has **`Trill-<version>-macos.zip`**: `Trill.app` and a page to read. Unpack it (a double-click in the
Finder does), drag `Trill.app` into your Applications folder, or any folder of yours, and start it from there. It
updates itself only where it can write: started where it was unpacked, macOS runs it from a copy that cannot be
written to, and it says so.

Trill is not signed with a developer's certificate nor notarised by Apple, so the first time you open it macOS
says that it could not check it, and opens nothing. Then, once:

1. Open **System Settings**, then **Privacy & Security**.
2. Scroll down to **Security**, where it says that Trill was blocked, and click **Open Anyway**.
3. macOS asks once more: click **Open Anyway** again, and give your password if it asks for it.

From then on Trill opens like any other application. This is Apple's own way for an application of a developer it
does not know ([Open a Mac app from an unknown developer](https://support.apple.com/guide/mac-help/open-a-mac-app-from-an-unknown-developer-mh40616/mac));
it changes nothing for any other application. `SHA256SUMS.txt` beside each release has the checksum of every file
of it.

**What Trill keeps, and where.** Its settings, what it fetches when you ask it to and your studies are in
`~/Library/Application Support/trill`, and what it can make again (the index of your sound library) in
`~/Library/Caches/trill`. Your songs are wherever you saved them. **Taking Trill off again** is moving `Trill.app`
to the Bin (and, where Trill has updated itself, the folder `.Trill.app.update` beside it, which the Finder shows
with Command-Shift-full stop); those two folders and your songs stay where they are.

## On Linux

Each release has **`trill-<version>-linux-x86_64.zip`**: the two programs, a page to read and the licences. Unpack it
into a folder of its own that you can write to, and start `trill` from there:

```sh
unzip trill-<version>-linux-x86_64.zip -d ~/Trill
~/Trill/trill
```

(A file manager's "Extract Here" makes the folder too; the archive says which files are programs, and `unzip` and
the desktop's own tools keep that.) Nothing is installed and nothing is asked of an administrator: no package, no
menu entry, and no file opens with Trill by itself.

It needs a C library (glibc) as new as the one it was built against, which `needs.txt` beside each release names,
and ALSA's library (`libasound.so.2`), which every desktop has; its sound goes through PipeWire or PulseAudio where
they are there. Its window is drawn through Wayland or X, and a plugin's own window through X (which a Wayland
desktop has beside itself, Xwayland). `SHA256SUMS.txt` beside each release has the checksum of every file of it.

**What Trill keeps, and where.** Its settings are in `~/.config/trill`; what it fetches when you ask it to and your
studies in `~/.local/share/trill`, and what it can make again (the index of your sound library) in `~/.cache/trill`,
unless the desktop says other places (`XDG_CONFIG_HOME` and the others). Your songs are wherever you saved them.
**Taking Trill off again** is deleting its folder (the update's folder `.update` is in it); those folders and your
songs stay where they are.

## A newer Trill

File > Setup has a tab, **Trill**, that says which Trill this is and what it knows of a newer one.

**Looking.** When Trill starts it looks whether a newer one is out. That fetches one small text, the announcement
of the newest release, which is the same for everyone: nothing about you, your songs or your computer is sent, and
it does not look again within six hours. "Look when Trill starts" on that tab switches it off; "Look now" looks
when you ask. `trill-mcp` never looks by itself.

**Hearing of it.** Where a newer Trill is out, the status bar says so in one line ("Trill 0.3.0 is out"). A click
opens the tab, which says what is new in it, how large it is and where it comes from. "Say no more of it" takes the
line away for that version; a version after it is said again.

**Fetching it.** Nothing but the announcement is fetched unasked. "Fetch" on the tab brings the build for your
machine (it can be stopped, and goes on from what had come), checks it against what its makers announced, and
keeps it ready beside the programs. Nothing has changed at that point, and "Put it away" discards it.

**When it goes in.** The next time you start `trill.exe` (on a Mac `Trill.app`, on Linux `trill`) while no other Trill is running, the newer one takes the
old one's place and starts, with whatever you started Trill with (a song you double-clicked opens in the new one).
It never goes in under a Trill that is running: Trill starts itself a second time to scan plugins, to render and
to study, and a program that changed under a running one would pair it with helpers of another version. So close
every Trill first, a `trill-mcp` that an AI client started included; until then the status bar goes on saying that
the newer one waits for a start.

**If the newer one does not start.** The Trill that put it in place waits to see it come up. If it ends before its
window is there, the old one is put back and goes on itself, and says so. If the power is cut while files are being
swapped, the next start puts back whatever had been moved, and Trill is as it was.

**Where Trill cannot update itself.** A copy that lies where it cannot write (a folder that needs an
administrator), and a kind of machine a release has no build for, are told so in a sentence that names the page
with the installer. Run the installer of the newer version over the old one: it keeps everything of yours. On a
Mac the sentence says to move `Trill.app` into a folder of yours (a copy started where it was unpacked is run by
macOS from one that cannot be written to), or to put the newer one in its place by hand. On Linux it says to unpack
the newer one into a folder of yours.

**On a Mac** an update replaces files inside `Trill.app`, its seal with them, and keeps what it needs meanwhile
beside it (`.Trill.app.update`, a folder the Finder does not show): nothing of it is ever left inside the
application, whose files are only those its seal holds.

**On Linux** the programs of an update are made programs (runnable) as it is staged: only the two files the
announcement names and Trill knows as its programs, after each was held to its checksum. Nothing an archive says of
a file is taken for it.

**What makes this safe.** The announcement is signed by Trill's makers with a key whose secret half is kept off the
network, and a Trill takes an announcement only under the keys it was built with. So whoever comes to control the
place releases are published at can announce nothing to it: at worst they withhold a release. The build is held to
the checksum the announcement gives, as it is fetched and again right before it goes in, and a Trill takes no
version that is not newer than itself.

A Trill you built yourself from the source is no release. It says so on the tab ("a build of your own"), looks
for nothing and is never replaced.

## Over MCP

The `setup` tool says the same: its report has `trill` (this version, a newer one with what is new in it, and
whether it is out, being fetched or ready), `look_for_newer` looks, and the part `trill` is fetched, stopped and
put away like any other part. An update never goes in while the server answers: it waits for the window's next
start. The assistant in the window reads what is known and neither looks nor fetches.
