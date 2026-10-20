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
janus save personal        # the account you're logged into now
# in Claude Code: /login with your other account
janus save work
```

## Switch by hand

```sh
janus                      # saved accounts; * marks the one in use
janus work                 # switch every open session to work
janus usage                # each account's 5-hour and weekly usage
janus status               # everything on one page (-w keeps it open)
```

`janus status` shows the account in use and why, each account's 5-hour and
weekly usage with when it resets, how long each saved login lasts before
you need to `/login` again, the watcher, and its recent switches.

Open sessions pick up the switch on their own: Claude Code reloads its login
when the file holding it changes.

## Switch on its own

```sh
janus auto primary personal    # use this one whenever it has room
janus auto secondary work      # and this one when it runs out
janus auto weekly on           # switch at 99% of the weekly limit
janus auto 5h on               # and/or at 99% of the 5-hour limit
janus auto 5h 95               # or at your own %
janus auto                     # settings, usage, and what the watcher did
```

Turning either option on starts a watcher that checks both accounts every
2 minutes and starts again when you log in. It moves to the secondary when
the primary reaches the threshold, and back as soon as the primary's limit
resets. Turning both options off stops it.

Each switch it makes shows a notification; `janus auto notify off` stops
them and `janus auto notify test` sends one to check they show up.

Picking an account by hand (`janus work`) holds it there: nothing switches
until `janus resume`.

## How it works

Claude Code keeps its login in two files. janus saves each account's copy
of those two entries in `~/.janus/` and swaps them in:

| File | Entry | What it is |
| --- | --- | --- |
| `~/.claude/.credentials.json` | `claudeAiOauth` | the tokens |
| `~/.claude.json` | `oauthAccount` | which account it is |

Everything else in those files, MCP logins included, is left alone and
shared. Usage comes from the same Anthropic endpoint Claude Code's `/usage`
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
