// /sharing: how an owner shares an agent with another person. Every claim here is checked against docs/invited.md,
// docs/kith-bot-guests.md, docs/memory-per-person.md, SECURITY.md, PRIVACY.md, kernel/bin/kith-auth, kernel/bin/kith-people
// and serve/host/src/api/{people,roles,botguests}.rs. Change the code, change this page.

import type { SitePage } from "../site";

export const PAGE: SitePage = {
  path: "/sharing",
  title: "Share an agent · Kith",
  description:
    "Invite someone to use one of your agents. It keeps running on your Mac, they use it in a browser, and you set their role and monthly limit.",
  kicker: "Sharing",
  h1: "Share an agent",
  lede: "Sharing gives one person the use of an agent you picked. The agent keeps running on your Mac, on your plan. They talk to it in a browser.",
  toc: [
    ["what", "What sharing is"],
    ["invite", "Invite someone"],
    ["they", "What they see"],
    ["roles", "Guest and member"],
    ["limits", "Spending limits"],
    ["reach", "What they can reach"],
    ["bots", "Bots they bring"],
    ["remove", "Remove someone"],
    ["offline", "When your Mac is off"],
    ["engine", "The engine"],
  ],
  body: `
<h2 id="what">What sharing is</h2>
<p>An invite lets one person chat with the agents you name, and nothing else on your Mac. The agent stays in its folder on your Mac and runs there. The person signs in on the web and installs nothing.</p>
<p>They get their own chats and their own memory with the agent. Yours stay yours. You choose a role for them, set how much they can spend each month, and can remove them at any time.</p>
<p>Sharing needs Kith running on your Mac and a Kith account, because their browser reaches your Mac through the Kith cloud. Messages on that path are encrypted end to end.</p>

<h2 id="invite">Invite someone</h2>
<h3>In the app</h3>
<ol>
  <li>In the agent's chat, open the <strong>More</strong> menu and choose <strong>Share access…</strong>. To give several agents at once, use Settings &gt; People &gt; <strong>Invite someone</strong>.</li>
  <li>Type their name, or pick someone you invited before.</li>
  <li>Choose <strong>Guest</strong> or <strong>Member</strong>. Guest is the safer start.</li>
  <li>Choose <strong>Create invite</strong>. The app shows a link and a QR code.</li>
  <li>Send them the link.</li>
</ol>
<p>The link is for one person. It works once and expires after 24 hours. The secret part comes after the <code>#</code>, so a browser never sends it to a server. Treat the link like a key until it is used.</p>
<p>An invite nobody has used yet is listed under <strong>Waiting to be used</strong> in Settings &gt; People. Cancel it there.</p>
<h3>From the terminal</h3>
<p>The same invite. Kith must be running on the Mac, because the host makes it:</p>
<pre><code><span class="c"># one agent, as a guest (the default)</span>
kith invite add Rex --agent reading

<span class="c"># several agents, a role after =</span>
kith invite add Rex --agent reading=member --agent video

<span class="c"># the invites this Mac made, and taking one back before it is used</span>
kith invite ls
kith invite revoke &lt;inviteId&gt;

<span class="c"># everyone you invited, and one person in full</span>
kith people
kith people show &lt;id&gt;</code></pre>
<p><code>kith invite add</code> prints the link to send.</p>

<h2 id="they">What they see</h2>
<p>They need a current Chrome, Safari or Firefox. Nothing else.</p>
<ol>
  <li>They open the link and confirm that they want to join your computer.</li>
  <li>They sign in with Google, GitHub or Apple.</li>
  <li>Kith joins them to your Mac and sets up that browser as one of their devices. You don't approve the browser. The invite already did.</li>
  <li>They see your computer, their chats there, and <strong>New chat</strong>.</li>
</ol>
<p>New chats list only the agents you gave them. Settings, your people, the marketplace and the terminal are not available to them.</p>

<h2 id="roles">Guest and member</h2>
<p>Each agent you share has a role for that person. You can give the same person a different role on each agent, and change it later in Settings &gt; People.</p>
<div class="wrap"><table>
<thead><tr><th></th><th>Guest</th><th>Member</th></tr></thead>
<tbody>
<tr><td>Chats with the agents you gave them</td><td>Yes</td><td>Yes</td></tr>
<tr><td>Their own memory with each agent</td><td>Yes</td><td>Yes</td></tr>
<tr><td>Something that needs permission</td><td>The agent asks you. You allow it or not in your app.</td><td>The agent works on its own, within the limits you set.</td></tr>
<tr><td>A command that runs code</td><td>Asks you</td><td>Asks you</td></tr>
<tr><td>Claude Code permission mode</td><td><code>default</code></td><td><code>acceptEdits</code></td></tr>
<tr><td>A monthly spending limit</td><td>Yes</td><td>Yes</td></tr>
<tr><td>Your settings, people, marketplace, terminal</td><td>No</td><td>No</td></tr>
</tbody></table></div>
<p>Guest is the default, in the app and in <code>kith invite add</code>.</p>

<h2 id="limits">Spending limits</h2>
<p>Each person has a monthly limit on each agent you gave them. It starts at $20 a calendar month. Their runs are counted at API prices, so on a subscription the number measures usage and is not a bill.</p>
<p>When someone reaches their limit, their message is not run. You get a card in that chat to add to the limit, and adding to it runs the message.</p>
<p>Change a limit in the agent's <strong>Share access…</strong> sheet, under <strong>Limits each month</strong>. You can also set no limit.</p>
<p>The sheet also shows what each person has spent on the agent this month.</p>

<h2 id="reach">What they can reach</h2>
<p>An invited person's run is boxed in. It reads and writes its own corner of the agent and is refused the rest.</p>
<div class="wrap"><table>
<thead><tr><th>In the agent's folder</th><th>Their run</th></tr></thead>
<tbody>
<tr><td><code>memory/people/&lt;id&gt;/</code></td><td>Their own memory. Reads and writes.</td></tr>
<tr><td><code>files/people/&lt;id&gt;/</code></td><td>Their own outputs. Reads and writes.</td></tr>
<tr><td><code>memory/shared/</code></td><td>What the agent knows about its craft, for everyone who uses it. Never facts about a person.</td></tr>
<tr><td>Your memory in <code>memory/</code></td><td>Denied</td></tr>
<tr><td>Another person's memory and files</td><td>Denied</td></tr>
</tbody></table></div>
<ul>
  <li>Their runs happen in a sandbox, and a run does not start if the sandbox is not available. Commands can't run outside it.</li>
  <li>Deny rules keep your secrets folders and keys out of reach. Your keys stay in the macOS Keychain.</li>
  <li>Their chats are stored on your Mac, beside yours. You can't read them in the apps. You see only the permission requests their agents send you.</li>
</ul>
<div class="note"><b>Worth telling them.</b> Their chats and memory live on your Mac, and the chat database is not encrypted. If you turn on backups, the memory your agents keep about them is copied to your private GitHub repositories with the rest.</div>

<h2 id="bots">Bots they bring</h2>
<p>A person you invited can bring a bot of their own, such as Grok, to ask your agents for them. The bot joins as a guest.</p>
<ul>
  <li>They make the bot's invite in their own Settings, under <strong>Your bots</strong>, and tick which of their agents it may ask. It never gets more than they have.</li>
  <li>The invite is a text they paste into their chat with the bot. It works once, for one bot, and expires after an hour.</li>
  <li>You get a card to allow or deny the bot before it starts. To skip the card for someone, turn on the <strong>Always allow</strong> switch for that person's bots in Settings &gt; People.</li>
  <li>The bot spends from its person's monthly limit. It has no limit of its own.</li>
  <li>You can see what their bots ask. This is different from their own chats, which you can't read in the apps.</li>
  <li>Removing a person cuts off their bots too. They can remove their own bot, and so can you.</li>
</ul>
<p>You can invite a bot of your own the same way, from Settings &gt; People &gt; <strong>Invite a bot</strong>.</p>

<h2 id="remove">Remove someone</h2>
<h3>Take one agent away</h3>
<p>In the agent's <strong>Share access…</strong> sheet, choose <strong>Remove</strong> beside their name. They stop being able to use that agent. Anything else you gave them stays.</p>
<h3>Remove the person</h3>
<p>In Settings &gt; People, choose <strong>Remove</strong> on their row. They lose access to your agents, their devices are signed out of your computer, and their running turns stop. Their bots stop too. Their memory and chats stay on your Mac.</p>
<p>Someone with a Kith account is removed through the Kith cloud, so it holds on every Mac of yours. If the cloud can't be reached, the change is refused and you try again.</p>
<h3>Undo</h3>
<p>Choose <strong>Undo</strong> on the banner, or <strong>Restore access</strong> on their row. They get back exactly what they had: the same agents, roles and devices.</p>
<p>Removing someone never deletes what the agent remembers about them. That memory stays in the agent's folder until you delete it by hand.</p>

<h2 id="offline">When your Mac is off</h2>
<p>The agents run on your Mac. While it is asleep, off or offline, they don't answer.</p>
<ul>
  <li>The people you invited see that your computer is offline, and that their chats come back when it is on.</li>
  <li>A join that is in progress waits and finishes when your Mac is back.</li>
  <li>Nothing is lost. Their chats are on your Mac and show again once the browser can reach it.</li>
</ul>

<h2 id="engine">The engine</h2>
<p>For people you invite, an agent runs on Claude Code, on your own Claude account. This is the one engine that can hold a run to the rules above, so Kith does not start an invited person's run on another one.</p>
<p>What they ask counts toward your plan, up to the limit you set for them.</p>
`,
  style: `td code{white-space:nowrap;overflow-wrap:normal}`,
  next: [
    ["/privacy", "Next", "Privacy"],
    ["/agents", "Read", "The agent format"],
  ],
};
