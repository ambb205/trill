# Keeping a meeting point

Two people at home, each behind their own router, cannot open a connection to each other: a router lets nothing in
that was not asked for. Both can connect *out*, though. A meeting point is a small program on a server that both
connect out to, and that passes what each says to the other, so that they can
[work in one song together](working-together.md). Anyone can keep one, for themselves and their friends. This page
is for whoever does: what the program is, how it is set up, what it lets in, and what its log says. Trill ships no
address of one: there is no meeting point unless someone keeps it.

## What it can and cannot do

- **It can neither read nor join a song.** The two programs prove the invitation to each other *through* it, end
  to end, by the handshake they use on a connection of their own, and everything after is encrypted under keys
  only the two of them have. The meeting point passes those bytes and counts them. It never has an invitation's
  secret, and neither of the two things it has of a meeting (what the meeting goes by, and what whoever asks for
  it has to show) can be worked back to it: each is a hash of the secret under a label of its own.
- **Only someone you gave a key can open a meeting on it; joining needs the invitation and nothing else.** A key
  never crosses the network: whoever opens a meeting says which key theirs is and proves the rest by a handshake
  that only someone who has it can finish. Your file of keys is a line a person, so that one person's can be taken
  away.
- **Whoever has not got the invitation cannot knock.** What a meeting goes by is said in the clear by whoever asks
  for it, so someone who listens on the way has it. With that alone they get nowhere: the meeting point asks a
  question, a number drawn by lot, and lets through only who answers with what the number and the invitation come
  to. A wrong answer is told that there is no such meeting, whether there is one or not, and whoever shares the
  song hears nothing of it.
- **Whatever reaches it is a stranger's, and bounded**: how many connections at once and from one address, how
  many meetings and how many people in one, how long anyone has to say what they came for, how long a silent
  connection is kept, how many bytes a key may pass in a day ([the limits](#the-limits)). It makes no room for
  what a stranger says a length is, and one connection's fault ends that connection.

What it does know: which key opened a meeting and when, the addresses that connect, and how many bytes pass. It is
not encrypted the way a web site is (there is no TLS): what is said to the meeting point itself (which meeting,
the question and its answer) can be read on the way, and is of no use to whoever reads it; a network that looks
for TLS on port 443 and passes nothing else will not pass it.

## The program

`trill-meet` is one file with nothing beside it: no audio, no window, nothing of Trill's songs. It runs on Linux
without privileges and keeps nothing but the file of keys you give it. It is built from Trill's repository:

```sh
rustup target add x86_64-unknown-linux-musl          # once
cargo build -p trill-meet --release --target x86_64-unknown-linux-musl
```

That makes `target/x86_64-unknown-linux-musl/release/trill-meet`: 1.1 MB, statically linked, so it runs on any
x86-64 Linux whatever is installed there. Built on Windows it needs one more word, because the linker is Rust's
own: `CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld` in the environment. That is how the one this page was
checked with was built, in fifteen seconds, and run on Ubuntu under WSL.

**On an ARM server** (a Raspberry Pi, an ARM instance of a hosting company) the same program is built for
`aarch64-unknown-linux-musl` in place of `x86_64-unknown-linux-musl`, or on the server itself with
`cargo build -p trill-meet --release`. Nothing in it is written for one processor: it and the three libraries it
uses (`snow` for the handshake, `sha2`, `getrandom`) are Rust throughout. This was not tried: nobody has built or
run it on ARM.

## Setting it up

On the server, with the file copied there:

```sh
sudo install -m 755 trill-meet /usr/local/bin/trill-meet
sudo useradd --system --home-dir /var/lib/trill-meet --create-home --shell /usr/sbin/nologin trill-meet
```

A key for each person who is to share songs through it. The key is shown once, when it is made; hand it over by a
way only they read. The file keeps what proves the key and whose it is, not the key:

```sh
sudo -u trill-meet trill-meet key Ada --keys /var/lib/trill-meet/keys
sudo -u trill-meet trill-meet keys --keys /var/lib/trill-meet/keys      # whose keys there are
sudo -u trill-meet trill-meet forget Ada --keys /var/lib/trill-meet/keys   # takes one away
```

A key that is taken away ends what it holds within a few seconds: its meetings, and the people in them. The
program reads the file again by itself when it has changed; nothing has to be restarted. Whoever can read the file
can open meetings on this server as the people in it (and cannot read a song by that), so it is the server's
keeper's alone to read: the program writes it that way.

A unit for systemd, `/etc/systemd/system/trill-meet.service`:

```ini
[Unit]
Description=Trill meeting point
After=network-online.target
Wants=network-online.target

[Service]
User=trill-meet
ExecStart=/usr/local/bin/trill-meet serve --port 443 --keys /var/lib/trill-meet/keys
# The one thing it is allowed that an ordinary user is not: a port under 1024.
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
LimitNOFILE=4096
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now trill-meet
journalctl -u trill-meet -f          # its log
```

The service writes nothing: its log is its standard output, which systemd keeps. Started again, it has forgotten
the meetings it held; the programs that held them open them again by themselves within a few seconds, under the
invitations they had, and whoever was in a song gets in again without doing anything.

**The port** is yours to choose. 443 passes most networks, which is why the unit above uses it, and is the one
privilege the program is given for it; with a port above 1024 it needs none (leave the two `Capability` lines
out). Without `--port` it is 8745, and that is what an address without a port means in Trill. Open the port for
TCP in the server's firewall and, at a hosting company, in its network rules.

**IPv4 and IPv6.** Without `--listen` the program takes both on the port. `--listen` names an address and port to
take instead and may be given more than once: `--listen 0.0.0.0:443 --listen [::]:443`, or one address of a
server that has several.

## What a friend types

Whoever is to share through it gets two things from you: the address (`meet.example.org:443`, or the server's
number and port) and their key. In Trill they choose **File, Share this song**, **A meeting point**, and give both
once; Trill keeps them in its settings. Whoever joins a song needs neither: the invitation they are handed carries
the meeting point's address.

## The limits

Each has a default that suits a small server, and a word to `serve` that sets it. One that turned someone away is
a line in the log and a sentence in the program that was turned away.

| What | By default | Set with |
|---|---|---|
| Connections at once, of every kind | 512 | `--connections` |
| Of them from one address (for IPv6: one network of 2^64 addresses) | 48 | `--from-one-address` |
| New connections one address may make in a minute | 240 | `--a-minute-from-one-address` |
| Meetings held at once | 64 | `--meetings` |
| Of them by one key | 4 | `--meetings-of-one-key` |
| People in a meeting, whoever shares among them | 16 | `--people` |
| Seconds anyone has to say what they came for | 10 | `--hello` |
| Seconds whoever shares has to take up a knock | 10 | `--answer` |
| Seconds a connection that passes nothing is kept | 120 | `--silent` |
| Seconds a meeting is remembered after its holder's connection was lost | 600 | `--away` |
| Megabytes the meetings of one key may pass in a day | 2048 | `--megabytes-a-day` |

A meeting of *n* people is 2*n* - 1 connections: one by which whoever shares holds it, and two for each of the
others (theirs, and the sharer's to them). So the defaults hold, say, thirty meetings of nine people, or sixty-four
of four. The bytes of a day are counted by the key and not by the meeting, because a new invitation is a new
meeting. What a song passes is small: about a hundred bytes a second for each person with nothing changing,
about six kilobytes a second under a drag, and the song itself once for whoever joins, which is megabytes for a
long one ([what it costs](working-together.md#what-it-costs)).

## What it takes

Measured with the Linux program under WSL on one machine (x86-64), and a second program beside it that held the
meetings open and passed a hundred kilobytes each way through every one of them
(`crates/trill-meet/examples/load.rs`):

| | Memory | Threads | Files open |
|---|---|---|---|
| Nothing connected | 0.9 MB | 3 | 4 |
| 64 meetings of four people (448 connections) | 10.5 MB | 451 | 452 |
| 30 meetings of nine people (510 connections; the default limit is 512) | 11.8 MB | 513 | 514 |
| After they had gone | 1.0 MB | 3 | 4 |

So a connection is a thread, one file, and about 23 kB while it is there, and nothing is kept of it after. The
default limits fit in the 1,024 files a program may have open on most systems, with room to spare (the unit above
allows 4,096 so that raising a limit does not run into it). It does nothing with what it passes but count it: 48
MB went through 240 connections in a third of a second there. On a server the size of the smallest a hosting
company rents, memory and processor are not what limits it; the bytes of a day are.

## What the log says

A line a happening, with the time in universal time in front (under systemd, the journal's own time too). The log
has the names you gave keys to, a few digits of a meeting to tell two apart, addresses and counts. It never has a
key, what proves one, either of the two things a meeting is known by, an invitation's secret, or a byte of what is
passed.

```text
2026-11-21 19:02:11Z trill-meet 0.1.0: a meeting point on [::]:443 (which takes IPv4 too, unless another program has the port for it)
2026-11-21 19:02:11Z its limits: 512 connections at once, 48 from one address and 240 new ones a minute; 64 meetings, 4 of one key, 16 people in one; ...
2026-11-21 19:02:11Z 2 keys are given in /var/lib/trill-meet/keys
2026-11-21 19:05:40Z the keys were read: 2 given (Ada, Ben)
2026-11-21 19:05:40Z Ada holds a meeting (3fa91c)
2026-11-21 19:06:02Z the meeting of Ada (3fa91c): someone is in it; 2 in it
2026-11-21 19:41:17Z the meeting of Ada (3fa91c): someone is out of it, one side closed it, after 9120 kB; 1 in it
2026-11-21 19:41:20Z the meeting of Ada (3fa91c) is over: its holder ended it
```

- `X holds a meeting`, `holds the meeting again` (its holder was away and is back), `is away: remembered for 600 s`
  (the connection was lost; whoever asks meanwhile is told so), `is forgotten`, `is over: its holder ended it`.
- `someone is in it` and `someone is out of it`, with why (`one side closed it`, `nothing passed for as long as a
  silent connection is kept`, `the key's bytes of the day are passed`, `the key was taken away`) and how much
  passed.
- `turned away (WORD), from ADDRESS: ...` with the limit that did it. Whoever is turned away a thousand times a
  second writes this line once and then a count every ten seconds (`turned away (full) 4312 more times in the 10 s
  after`). `turned away (answer)` is someone who asked for a meeting that there is without its invitation;
  `turned away (key)` someone who named a key that is none of yours, or had not got it.
- `the key of X has passed its N MB of the day`, once a day at the most, and `the key of X was taken away: what it
  held is ended`.
- `a connection could not be taken: ...`: the system has no files left for another connection. The program says
  when it starts if the system lets it have fewer than its limits need (`LimitNOFILE` in the unit).

## What it does not do

- It is not encrypted as a web site is: no TLS, and so no certificate to keep. What the two programs say to each
  other is encrypted end to end all the same.
- It introduces nobody: the two programs stay connected through it for as long as they are in the song. A direct
  way between them, where there is one, is not built.
- It carries the song and nothing else: audio files do not travel yet.
- It cannot take one person's leave away from a meeting, only a key from whoever holds it.
- It keeps no accounts and nothing on disk but your file of keys.
