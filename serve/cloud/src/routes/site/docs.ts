// /docs: "Get started". From nothing to a working agent: install, make one, talk to it, give it a skill, a tool and an
// account, schedule it, reach it from a chat channel, read its cost, roll a change back. Every command here is checked
// against kernel/bin (kith, kith-new, kith-ask, kith-skills, kith-auth, kith-keys, kith-sched, kith-cost, kith-pkg).

import type { SitePage } from "../site";

const GITHUB = "https://github.com/Ying-Kai-Liao/kith";

export const PAGE: SitePage = {
  path: "/docs",
  title: "Get started · Kith",
  description:
    "Install Kith, make your first agent, give it a skill, a tool and an account, run it on a schedule and roll back a change.",
  kicker: "Docs",
  h1: "Get started",
  lede: "An agent is a folder on your Mac with instructions, skills and tools. This page takes you from nothing to one that works, in about ten minutes.",
  toc: [
    ["need", "What you need"],
    ["install", "Install"],
    ["first", "Make your first agent"],
    ["talk", "Talk to it"],
    ["skill", "Give it a skill and a tool"],
    ["account", "Give it an account"],
    ["schedule", "Run it on a schedule"],
    ["channels", "Reach it from a chat channel"],
    ["cost", "See what it cost"],
    ["rollback", "Roll back a change"],
  ],
  body: `
<h2 id="need">What you need</h2>
<ul>
  <li>A Mac, Apple silicon or Intel. Agents run on macOS only.</li>
  <li>Claude Code, Codex or pi, installed and signed in. Agents run on Claude Code unless you choose another engine.</li>
  <li>git. Each agent is a git repo.</li>
  <li>The GitHub CLI, <code>gh</code>, if you want to share an agent or back up to GitHub.</li>
</ul>
<p>Kith is free and open source. Each run uses your own plan with the provider of the engine it runs on.</p>

<h2 id="install">Install</h2>
<p>Kith is an app and the <code>kith</code> command. The app is where you talk to agents and answer what they ask. The command does the same work from a terminal, a script or another session.</p>
<ul>
  <li><strong>The app.</strong> Get it from <a href="/download">Download</a>. It installs the version of the command it needs under <code>~/.kith/cli</code>.</li>
  <li><strong>The command.</strong> Run the installer, then the check. The steps and other ways to install are on <a href="/install">Install</a>.</li>
</ul>
<pre><code>curl -fsSL https://kithbot.app/install.sh | sh
kith doctor          <span class="c"># checks the install and says what to fix</span></code></pre>
<p>The installer ends with <code>kith init</code>. It makes your folders, asks whether to back up to private GitHub repos, and offers builder, a starter agent that sets up other agents with you. Run <code>kith init</code> again any time. It only does what is missing.</p>

<h2 id="first">Make your first agent</h2>
<p>One command makes the folder. The name is lowercase letters, digits and dashes. The description is one line.</p>
<pre><code>kith new reading "tracks what I read"
kith ls              <span class="c"># your agents</span>
kith paths           <span class="c"># where the folders are on this Mac</span></code></pre>
<p>That creates <code>agents/reading/</code>. Inside it, <code>role/</code> is a git repo with <code>agent.md</code>, <code>capabilities.yaml</code>, <code>settings.json</code> and <code>skills/</code>. Beside <code>role/</code> sits <code>memory/</code>, which stays with you.</p>
<p>Open <code>role/agent.md</code> and replace the placeholder lines with what the agent is for and how it should work:</p>
<pre><code>---
name: reading
description: tracks what I read
---

You keep track of what I read. Save each article with one line on why it
matters. On Fridays, list what I finished.</code></pre>
<p>There is nothing to build or reload. The next run uses your edit. To read the full prompt a run would get, without running it:</p>
<pre><code>kith reading --print</code></pre>
<p>If you took builder during setup, you can ask it instead: <code>kith builder</code> starts a session that writes the agent with you. Every field of <code>agent.md</code> is on <a href="/agents">The agent format</a>.</p>

<h2 id="talk">Talk to it</h2>
<p>Every call is a fresh session that reads the agent's memory first, so it picks up where the last one stopped.</p>
<h3>In the app</h3>
<p>Choose the agent and write to it. When it needs your permission or a decision, it asks with a card in the same chat.</p>
<h3>In a terminal</h3>
<pre><code>kith reading                               <span class="c"># an interactive session</span>
kith reading "save this: https://example.com/essay"   <span class="c"># one task, then it exits</span></code></pre>
<h3>From a script or another session</h3>
<p><code>kith ask</code> runs the agent the same way, prints only its answer and keeps the conversation.</p>
<pre><code>kith ask reading "what did I finish this month?"
kith ask reading -c "and last month?"      <span class="c"># continue, from the same folder</span>
kith ask reading --as books "start a list" <span class="c"># a named conversation, continued from anywhere</span>
kith ask ls                                <span class="c"># the conversations kept</span></code></pre>
<p>Add <code>--json</code> for the answer, session and cost as one object. Add <code>--bg</code> to get an id back at once and collect the answer later with <code>kith ask --wait ID</code>.</p>
<h3>Inside Claude Code</h3>
<p>The installer adds a Claude Code plugin. In any session, your agents appear as <code>kith:reading</code> and so on. Ask for one by name, or type <code>/kith reading</code> followed by the task. Your main session sees one line per agent. An agent's skills load only when it is called.</p>

<h2 id="skill">Give it a skill and a tool</h2>
<p>A skill is a folder with a <code>SKILL.md</code> that says how to do one job. A tool is a script the skill runs. Both live in the role, so they are versioned and shared with the agent.</p>
<pre><code>agents/reading/role/
  agent.md
  skills/weekly-list/SKILL.md
  tools/save.py</code></pre>
<p>Write the skill:</p>
<pre><code>---
name: weekly-list
description: Build the Friday list of what was finished this week.
---

1. Read this week's saved articles.
2. Group them by subject, one line each.
3. Send the list.</code></pre>
<p>Then name it in the skills table of <code>agent.md</code>, so the agent knows when to read it. The agent reads a skill only when the task calls for it.</p>
<pre><code>| Task | Skill |
|---|---|
| The Friday list | \`weekly-list/SKILL.md\` |
| Saving an article | run \`{{AGENT_DIR}}/tools/save.py &lt;url&gt;\` |</code></pre>
<p><code>{{AGENT_DIR}}</code> is filled in at run time with the folder of the version that is running.</p>
<p>To use a skill someone published instead of writing one, link it. The id is a name in <a href="https://github.com/anthropics/skills">github.com/anthropics/skills</a>, or <code>owner/repo/path</code> for any GitHub repo.</p>
<pre><code>kith skills available            <span class="c"># the anthropics/skills catalog</span>
kith reading skill add pdf       <span class="c"># link one, pinned to the commit it is at now</span>
kith reading skills              <span class="c"># what the agent has: its own, learned, linked</span></code></pre>
<p>If a tool runs a program your Mac may lack, list it under <code>requires:</code> in <code>capabilities.yaml</code>, one per line as <code>name: install command</code>. <code>kith deps reading</code> shows what is missing and <code>kith deps reading --install</code> adds it.</p>
<div class="note"><b>Edits are checked before they run.</b> On the next run Kith checks the role: the frontmatter, the JSON files, and that every skill named in <code>agent.md</code> exists. A version that passes is committed to the role's repo. A version that fails never runs. The last good one runs, and the reason is printed.</div>

<h2 id="account">Give it an account</h2>
<p>An agent carries no keys. It names the services it needs, and you grant it one of your accounts. The key is stored in your Keychain and reaches only the process that uses it.</p>
<h3>1. The agent names the service</h3>
<p>In <code>role/capabilities.yaml</code>, as a one-line list:</p>
<pre><code>needs: [notion]</code></pre>
<p>A service is a small file that says which secrets it takes, by name. Kith ships some, such as <code>notion</code>. An agent can bring its own in <code>role/services/&lt;name&gt;.json</code>.</p>
<h3>2. You grant an account</h3>
<p>An account is a profile named <code>service/account</code>. You can keep several per service, such as <code>notion/library</code> and <code>notion/work</code>.</p>
<pre><code>kith auth set notion/library NOTION_API_KEY     <span class="c"># prompts for the key, stores it in the Keychain</span>
kith auth grant reading notion notion/library   <span class="c"># reading may use notion as that account</span>
kith auth                                       <span class="c"># who needs what, who has what</span></code></pre>
<p>A tool then runs with the key in its own environment and nowhere else:</p>
<pre><code>kith auth exec --for reading notion -- python3 tools/save.py</code></pre>
<h3>Or let the agent ask</h3>
<p>With the app running, an agent that lacks a key asks for it. A card appears in your chat with that agent. You pick an account you already have, or type the key into a private field. Kith saves it to the Keychain, grants it to the agent, and the agent carries on. It is told the name of the key and never sees the value. If you say no, nothing is saved.</p>
<p>To open the same card yourself:</p>
<pre><code>kith keys add notion --profile library --for reading</code></pre>
<p>Take a grant away with <code>kith auth revoke reading notion</code>. Grants keep each agent to the accounts it needs. They are not a sandbox: an agent with a shell can read what your user can.</p>

<h2 id="schedule">Run it on a schedule</h2>
<p>A schedule runs the same one-off task you would type, at a set time. Turn the scheduler on once per Mac, then add jobs.</p>
<pre><code>kith schedule --install          <span class="c"># once per Mac</span>
kith schedule add reading "0 8 * * 5" "List what I finished this week and send it"
kith schedule                    <span class="c"># every job, its next run and how the last one went</span>
kith schedule run reading ID     <span class="c"># run one now</span>
kith schedule log reading ID     <span class="c"># the last runs and their output</span>
kith schedule pause reading ID   <span class="c"># also resume and rm</span></code></pre>
<p>The time is five cron fields, in your Mac's local time: minute, hour, day of month, month, day of week. <code>@hourly</code>, <code>@daily</code>, <code>@weekly</code> and <code>@monthly</code> also work. If the Mac slept through a time, the job runs once when it wakes, as long as the miss was under a day.</p>
<p>Nobody is there to approve a tool call in a scheduled run, so a call that would need approval is refused. The agent's own allow and deny rules still apply. Jobs are kept beside the role in <code>agents/reading/jobs.json</code>, so they are never part of what you share.</p>

<h2 id="channels">Reach it from a chat channel</h2>
<p>You can message your agents from Telegram, Slack or Discord. The bot runs on your Mac, inside the host that comes with the app, so it answers while your Mac is on. You bring a bot token from the chat service and grant it to <code>serve</code>, which is the part of Kith that runs the channels.</p>
<h3>Telegram</h3>
<pre><code>kith auth set telegram/home TELEGRAM_BOT_TOKEN
kith auth grant serve telegram telegram/home</code></pre>
<h3>Discord</h3>
<pre><code>kith auth set discord/home DISCORD_BOT_TOKEN
kith auth grant serve discord discord/home</code></pre>
<p>The bot needs the Message Content intent. It works in one server, where it makes a channel for each agent.</p>
<h3>Slack</h3>
<pre><code>kith auth set slack/work SLACK_BOT_TOKEN
kith auth set slack/work SLACK_APP_TOKEN
kith auth grant serve slack slack/work</code></pre>
<p>The Slack app runs in Socket Mode, so it needs no public address. The scopes and events it needs are listed in the <a href="${GITHUB}/blob/main/serve/host/README.md">host README</a>.</p>
<h3>Link your own chat account</h3>
<p>The bot serves only chat accounts that are linked to a person Kith knows. In the app's settings, choose <strong>Link Telegram or Discord</strong> to get a one-time code, then send it to the bot:</p>
<pre><code>/link CODE</code></pre>
<p>Then write to an agent by name. In a thread, the agent that answered last keeps answering.</p>
<pre><code>@reading what did I finish this month?
/default reading     <span class="c"># this chat goes to reading unless you name another</span>
/stop                <span class="c"># stop the run in progress</span></code></pre>
<p>On Slack, commands start with <code>!</code> in place of <code>/</code>, as in <code>!link CODE</code>.</p>

<h2 id="cost">See what it cost</h2>
<p>Kith keeps a ledger of every run with its agent, version, model and effort.</p>
<pre><code>kith cost agents            <span class="c"># agents side by side, this month</span>
kith cost agents week       <span class="c"># or today, month, all</span>
kith cost agent reading     <span class="c"># one agent: by day, by version, by model, its costliest runs</span>
kith cost csv               <span class="c"># every request, for a spreadsheet</span></code></pre>
<p>Amounts are list API prices. On a subscription they show usage, not a bill. Runs on Claude Code are recorded per request. Runs on another engine are recorded as one row per run.</p>
<p>To cap an agent, give it a monthly limit in dollars: <code>kith reading set limit 20</code>. <code>kith reading set limit none</code> removes it.</p>

<h2 id="rollback">Roll back a change</h2>
<p>Every version of a role that passes its checks is a commit in the role's repo. That includes changes the agent made itself, such as a skill it learned.</p>
<pre><code>kith history reading             <span class="c"># every committed version, newest first</span>
kith restore reading COMMIT      <span class="c"># make the role what it was then, as a new commit</span></code></pre>
<p>A restore is itself a commit, so you undo it with another restore. The next run uses the restored version. Runs already in progress finish on the version they started with.</p>
<p>To step back without changing any files, pin the previous good version, and unpin when you are ready to follow your edits again:</p>
<pre><code>kith rollback reading
kith unpin reading</code></pre>
<p>Restoring a role never touches the agent's memory, data or files. They live beside the role, not in it.</p>
`,
  next: [
    ["/agents", "Reference", "The agent format"],
    ["/sharing", "Next", "Share an agent with someone"],
  ],
};
