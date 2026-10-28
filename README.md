# janus: two Claude Code accounts, one switch

Switch which account Claude Code is logged into without closing any
sessions, by hand or on its own when one account runs low.

```sh
curl -fsSL https://raw.githubusercontent.com/joeboylson/janus/main/install.sh | sh
```

That puts `janus` in `~/.local/bin`. Run the same line again to update. It
works on macOS (Apple chip or Intel) and Linux; starting the watcher at login
is macOS only.

## Set up

```sh
janus setup
```

It walks you through it, one question at a time:

1. It saves the account Claude Code is logged into now, and asks what to call it.
2. It asks you to run `/login` in Claude Code with your other account, waits,
   and notices when it lands.
3. It asks which account is the primary, which limits to switch on (weekly,
   5-hour, or both) and at what %, and whether to notify you.
4. It offers to switch to the primary.

Run it again any time to change things. Every command works this way: it asks
for whatever you leave out, and in a script, where there's no terminal to ask
in, it says which flag or name is missing.

## Every day

```sh
janus status               # everything on one page, in color
janus status --watch       # the same, kept on screen; q quits
janus work                 # switch every open session to work (or: janus use)
janus resume               # after switching by hand, let it switch on its own again
```

`janus status` shows the account in use and why, each account's 5-hour and
weekly usage as a bar (green, then yellow from 70%, red from 90% or past the
switch point) with when it resets, how long each saved login lasts before you
need to `/login` again, the watcher, and its recent switches. `NO_COLOR=1`
turns the color off.

On a Mac, open sessions follow a switch within about 30 seconds: janus saves
the login in the Keychain, which Claude Code reads again every 30 seconds.
On Linux the login lives in a file, which open sessions only read when they
start or when their login stops working, so a switch reaches new sessions
right away but open ones only once they restart. `janus status` says which
applies.

## One session on one account

```sh
janus run work             # a Claude Code session that stays on work
janus run work --resume    # anything after the name goes to claude
janus run --new-token work # replace its saved long-lived login
```

`janus run` starts `claude` on that account and keeps it there: switching by
hand, `janus resume` and the watcher move every other session, never this one.
The first time for each account, it runs `claude setup-token`, which opens
your browser; sign in there as that account and paste the token it prints.
janus saves it, and after that `janus run work` asks nothing. The token lasts
a year, and janus warns you a month before it runs out.

The session hands Claude Code the token through `CLAUDE_CODE_OAUTH_TOKEN`,
which it uses over the shared login. Its usage counts against that account,
though `/status` inside it may still name the other one.

## Switch on its own

`janus setup` turns it on. Without the questions:

```sh
janus auto on --primary personal --secondary work --weekly 99 --five-hour 99
janus auto on --five-hour 95   # change one setting
janus auto off
janus auto show                # its settings and what the watcher is doing
janus auto test                # send a test notification
```

While it's on, a watcher checks both accounts every 2 minutes and starts
again when you log in. It moves to the secondary when the primary reaches
the threshold, and back as soon as the primary's limit resets, with a
notification each time unless `--notify off`. Picking an account by hand
(`janus work`) holds it there: nothing switches until `janus resume`.

## Accounts

```sh
janus account add              # save the login Claude Code has now, or log in to another first
janus account list
janus account remove work
janus logs --watch             # what the watcher has done
```

## How it works

Claude Code keeps its login in two places. janus saves each account's copy
of those two entries in `~/.janus/` and swaps them in:

| Where | Entry | What it is |
| --- | --- | --- |
| Mac: the Keychain item `Claude Code-credentials`; Linux: `~/.claude/.credentials.json` | `claudeAiOauth` | the tokens |
| `~/.claude.json` | `oauthAccount` | which account it is |

On a Mac, Claude Code reads the Keychain item first and falls back to
`~/.claude/.credentials.json` only while there's no item, for example when it
logged in while the Keychain was locked. janus reads them in the same order
and always saves to the Keychain. With no item yet, it makes one from the
whole file, so MCP logins come along, and keeps the file in step. If the
Keychain is locked, janus saves to the file and says that open sessions
won't follow.

Everything else, MCP logins included, is left alone and shared. janus works
with the default `~/.claude` only, not a `CLAUDE_CONFIG_DIR`. Usage comes from the same Anthropic endpoint Claude Code's `/usage`
reads. That endpoint isn't publicly documented, so it may change.

Logins never leave your computer except to Anthropic, for that usage check.

## Build from the code

You need [Rust](https://rustup.rs).

```sh
git clone https://github.com/joeboylson/janus.git && cd janus
./install.sh               # builds and installs from this copy
cargo test
```

Pushing a tag like `v0.2.0` builds a release for every supported computer
(`.github/workflows/release.yml`); the install line downloads the latest.
