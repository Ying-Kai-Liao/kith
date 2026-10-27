# Joining someone's Kith

Someone runs Kith on their Mac and invited you to use some of their agents. This page explains what the link does, how to
join, and what to expect. You don't need to install anything if you use the web client.

## The link

An invite comes from the owner's Mac (Settings > People, `createInvite` in `serve/host/src/api/people.rs`). It is good
for **24 hours** and for **one account**: once someone joins with it, it can't be used again. If it has expired or was
used, ask for a new one.

The owner can share it in one of three forms:

| Link | Opens |
|---|---|
| `https://kith-web-dev.lykevin890919.workers.dev/join#…` | The web client, in your browser. Only invites from a Kith Dev Mac include it, because the web client runs on the development instance for now. |
| `https://kithbot.app/i/…` | A page that opens the Kith app (`kith://invite…`). If you don't have the app, ask the person who invited you how to get it: it isn't in the App Store yet ([install.md](install.md)). |
| `kith://invite?…` | The Kith app directly, on a Mac or an iPhone. |

The secret part of the link comes after the `#`, so your browser never sends it to a server. Treat the link like a key
until you've used it.

## Joining on the web

1. Open the link and confirm **Join a computer on Kith?**
2. Sign in with Google or GitHub. Sign in with Apple doesn't work on the development instance.
3. Kith joins you to the owner's computer, sets up this browser as one of your devices, and connects. The steps show
   as they happen. The owner doesn't have to approve the browser: the invite already did that.
4. You see the owner's computer, the chats you have there, and **New chat**.

Use a current Chrome, Safari or Firefox. The web client keeps its device key in the browser where it can't be copied out,
and an older browser that can't do that gets a page asking you to update it.

## What you can do

- **Chat with the agents the owner gave you.** New chats list only those agents. Settings, the owner's people, the
  marketplace and the terminal aren't available to you.
- **Each agent has a role for you**, chosen by the owner:
  - **Guest** (the default): the agent asks the owner before it does anything that needs permission. The owner gets
    the request in their app and allows it or not.
  - **Member**: the agent works on its own within the limits the owner set.
- **There's a monthly limit.** Your runs on each agent count toward a limit the owner sets, at API prices. It starts
  at $20 a month. When you reach it, your message isn't run and the owner gets a card to raise the limit or leave it.
- **The owner can see and end your access** at any time, from their People settings.

## When the Mac is off

The agents run on the owner's Mac. When it's asleep, off or offline:

- Your chat list says the computer is offline and that your chats come back when it's on.
- A join that's in progress waits and finishes when the Mac is back. You can also tap Try again.

Nothing is lost. Your chats are on the owner's Mac, and the web client shows them again once it can reach it.

## Using an app instead of the browser

The Kith app for Mac or iPhone opens the same invites (`kith://invite`, or the `kithbot.app/i/…` page). Accounts in
the apps show only in builds made with the Kith account key, so for now the web client is the easiest way in.
