// /agents: "The agent format". What an agent is on disk: the folder, agent.md and its frontmatter, the other role
// files, skills, tools, memory, data and files, what is in git, sharing and installing, versions, engines. Checked
// against kernel/bin (kith, kith-new, kith-pkg, kith-skills, kith-state), kernel/lib/agents.gitignore and PROTOCOL.md.

import type { SitePage } from "../site";

export const PAGE: SitePage = {
  path: "/agents",
  title: "The agent format · Kith",
  description:
    "What a Kith agent is on disk: the folder layout, agent.md and its fields, skills, tools, memory, what is in git, sharing, versions and engines.",
  kicker: "Reference",
  h1: "The agent format",
  lede: "An agent is a folder. Its role is a git repo you can share. Its memory, data and files sit beside the role and stay with you.",
  toc: [
    ["folder", "The folder"],
    ["agent-md", "agent.md"],
    ["role-files", "Other role files"],
    ["skills", "Skills"],
    ["tools", "Tools"],
    ["memory", "Memory, data and files"],
    ["git", "What is in git"],
    ["share", "Share and install"],
    ["versions", "Versions"],
    ["engines", "Engines"],
  ],
  style: `
td:first-child{white-space:nowrap}
@media (max-width:720px){td:first-child{white-space:normal}}
`,
  body: `
<h2 id="folder">The folder</h2>
<p>Every agent lives in the agents folder, one folder each. <code>kith paths</code> prints where that is on your Mac. <code>kith new &lt;name&gt; "&lt;description&gt;"</code> makes one.</p>
<pre class="tree"><code>agents/reading/
  role/                     <span class="c">what the agent is: its own git repo, shareable</span>
    agent.md                <span class="c">who it is and how it works</span>
    capabilities.yaml       <span class="c">the services and programs it needs, by name</span>
    settings.json           <span class="c">Claude Code settings for its runs</span>
    skills/                 <span class="c">its skills, one folder each</span>
    skills/learned/         <span class="c">skills it wrote itself</span>
    tools/                  <span class="c">scripts its skills run</span>
  memory/                   <span class="c">what it remembers: yours only</span>
    MEMORY.md               <span class="c">the long-term index, loaded on every call</span>
    projects/&lt;id&gt;.md        <span class="c">its notes on one project</span>
    sessions/&lt;id&gt;.md        <span class="c">where the last call on that project stopped</span>
  data/                     <span class="c">its databases</span>
  files/                    <span class="c">what it made</span>
  jobs.json                 <span class="c">its schedules</span></code></pre>
<p>The split is the point of the format. The role says what the agent is. Everything beside it is what the agent has lived. Sharing an agent shares the role and nothing else, and updating or restoring a role never touches memory.</p>

<h2 id="agent-md">agent.md</h2>
<p><code>agent.md</code> is the agent's instructions: a short frontmatter block, then plain markdown. The markdown becomes the start of the prompt for every run.</p>
<pre><code>---
name: reading
description: tracks what I read
model: sonnet
effort: medium
harness: claude
---

You keep track of what I read. Save each article with one line on why it
matters. On Fridays, list what I finished.

## Your skills
| Task | Skill |
|---|---|
| The Friday list | \`weekly-list/SKILL.md\` |</code></pre>

<h3>Frontmatter fields</h3>
<p>Each field is one line, <code>key: value</code>. Only <code>description</code> is required.</p>
<dl>
  <dt>name</dt><dd>The agent's name. Lowercase letters, digits and dashes, starting with a letter. <code>kith new</code> writes it to match the folder.</dd>
  <dt>description</dt><dd>One line on what the agent is for. Required: a role without it fails its checks. It is the line your main session sees for this agent, and what <code>kith ls</code> and a shared repo show.</dd>
  <dt>model</dt><dd>A model name or alias of the agent's engine, such as <code>sonnet</code>. Leave it out to use your own default.</dd>
  <dt>effort</dt><dd>Reasoning effort: <code>low</code>, <code>medium</code>, <code>high</code>, <code>xhigh</code> or <code>max</code>. Any other value fails the checks. Leave it out to use your own default.</dd>
  <dt>harness</dt><dd>The engine the agent runs on. Leave it out for Claude Code. See <a href="#engines">Engines</a>.</dd>
  <dt>display_name</dt><dd>The name the agent posts under in Discord, when you want one other than <code>name</code>.</dd>
  <dt>memory_cap</dt><dd>The most characters of memory loaded at the start of a conversation. The default is 8000.</dd>
</dl>
<p>You can edit these by hand, or let Kith write and commit the line:</p>
<pre><code>kith reading set model sonnet        <span class="c"># or: default, to remove the line</span>
kith reading set effort high
kith reading set harness codex</code></pre>

<h3>Placeholders</h3>
<p>Paths differ from Mac to Mac, so a role never writes one down. Use these in <code>agent.md</code> and <code>settings.json</code>. They are filled in when a run starts. A placeholder Kith does not know fails the checks.</p>
<div class="wrap"><table>
<thead><tr><th>Placeholder</th><th>Becomes</th></tr></thead>
<tbody>
<tr><td><code>{{AGENT_DIR}}</code></td><td>The version of the role this run is pinned to. Read skills and run tools from here.</td></tr>
<tr><td><code>{{PACKAGE}}</code></td><td>The live <code>role/</code> folder, where the agent writes a learned skill.</td></tr>
<tr><td><code>{{HOME_DIR}}</code></td><td>The agent's own folder: memory, data and files.</td></tr>
<tr><td><code>{{HOME_REAL}}</code></td><td>The same folder with symlinks resolved. Use it in permission rules.</td></tr>
<tr><td><code>{{AGENTS}}</code></td><td>The agents folder, to reach another agent's tools.</td></tr>
<tr><td><code>{{STATE}}</code></td><td>Kith's state folder: your profile and the shared project notes.</td></tr>
<tr><td><code>{{USER_HOME}}</code></td><td>Your home folder.</td></tr>
<tr><td><code>{{AGENT}}</code></td><td>The agent's name.</td></tr>
</tbody></table></div>

<h2 id="role-files">Other role files</h2>
<dl>
  <dt>capabilities.yaml</dt><dd>What the agent needs, by name and never by credential. <code>needs: [notion, jev-browser]</code> lists services as a one-line list. <code>requires:</code> lists programs it runs, one per line as <code>ffmpeg: brew install ffmpeg</code>.</dd>
  <dt>settings.json</dt><dd>Claude Code settings merged over your global ones for this agent's runs: permissions, hooks and plugins. <code>permissions.defaultMode</code> is the mode the author suggests. It applies to runs on Claude Code.</dd>
  <dt>mcp.json</dt><dd>Optional. MCP servers that belong to this agent alone.</dd>
  <dt>skills.json</dt><dd>Optional. Skills linked from other GitHub repos, each pinned to a commit.</dd>
  <dt>services/</dt><dd>Optional. The definition of a service Kith does not ship, as <code>services/&lt;name&gt;.json</code>: its kind and the names of its secrets.</dd>
</dl>
<p>An agent carries no accounts. It names a service in <code>needs:</code>, and each person who runs it grants one of their own accounts with <code>kith auth grant</code>. Two people can run the same role with their own logins. <a href="/docs#account">Give it an account</a> has the steps.</p>

<h2 id="skills">Skills</h2>
<p>A skill is a folder with a <code>SKILL.md</code>: a <code>name</code> and <code>description</code> in its frontmatter, then the steps. Skills are not loaded up front. <code>agent.md</code> lists them in a table, and the agent reads one when the task calls for it. That keeps each run small, and it keeps one agent's skills out of every other agent's context.</p>
<div class="wrap"><table>
<thead><tr><th>Kind</th><th>Where</th><th>Shared with the role</th></tr></thead>
<tbody>
<tr><td>Its own</td><td><code>role/skills/&lt;name&gt;/SKILL.md</code></td><td>Yes</td></tr>
<tr><td>Learned</td><td><code>role/skills/learned/&lt;name&gt;/SKILL.md</code></td><td>Yes</td></tr>
<tr><td>Linked</td><td>Named in <code>role/skills.json</code>, fetched into <code>role/skills/&lt;name&gt;/</code></td><td>The link is. The files are fetched again on install.</td></tr>
<tr><td>Private</td><td><code>agents/&lt;name&gt;/skills/learned/</code>, beside the role</td><td>No</td></tr>
</tbody></table></div>
<h3>Learned skills</h3>
<p>When an agent solves something it will need again, it writes the solution down as a skill in <code>role/skills/learned/</code>. That is an edit to the role like any other: checked on the next run, committed, and yours to read, change or <a href="#versions">roll back</a>.</p>
<p>A skill you would rather not share goes in the private folder beside the role. To share it later, move its folder into <code>role/skills/learned/</code>.</p>
<h3>Linked skills</h3>
<p>A skill from another GitHub repo is linked, not copied. <code>skills.json</code> records its repo, path and commit, and Kith fetches only that folder. To upgrade one, change its commit.</p>
<pre><code>kith skills available              <span class="c"># the catalog at github.com/anthropics/skills</span>
kith reading skill add pdf         <span class="c"># or owner/repo/path, or owner/repo/path@ref</span>
kith reading skill remove pdf
kith reading skills                <span class="c"># own, learned and linked</span></code></pre>
<p>If <code>agent.md</code> names a <code>SKILL.md</code> that does not exist, or a linked skill has not been fetched, the role fails its checks and the last good version runs.</p>

<h2 id="tools">Tools</h2>
<p>A tool is a script in <code>role/tools/</code> that a skill tells the agent to run. It is an ordinary file in the role: versioned, shared and installed with it. Refer to it as <code>{{AGENT_DIR}}/tools/&lt;file&gt;</code> so the path holds on any Mac.</p>
<ul>
  <li>A program the tool runs goes in <code>requires:</code>. Installing the agent lists what is missing and offers to add it. <code>kith deps &lt;agent&gt; --install</code> does it later.</li>
  <li>A key the tool needs comes from a granted service, never from the role. The tool runs under <code>kith auth exec --for &lt;agent&gt; &lt;service&gt; -- CMD</code>, which puts the secret in that process only.</li>
  <li>An MCP server the agent needs is a service in <code>needs:</code>. A run starts only the servers the agent both needs and is granted.</li>
</ul>

<h2 id="memory">Memory, data and files</h2>
<p>These three folders are what the agent has lived. They are never part of the role.</p>
<dl>
  <dt>memory/</dt><dd>Markdown the agent writes for itself. <code>MEMORY.md</code> is the long-term index and loads on every call. <code>projects/&lt;id&gt;.md</code> holds its notes on one project. <code>sessions/&lt;id&gt;.md</code> holds where the last call on that project stopped, which is how a fresh session picks up the thread.</dd>
  <dt>data/</dt><dd>The agent's databases.</dd>
  <dt>files/</dt><dd>What the agent made, by project: renders, images, exports.</dd>
</dl>
<p>Memory is the agent's notes, not the truth. Current facts such as a calendar or a balance are read from their source each time. To search what your agents wrote down:</p>
<pre><code>kith memory search &lt;words&gt;</code></pre>
<p>When you invite someone to an agent, their runs get a memory of their own under <code>memory/people/&lt;id&gt;/</code> and their own outputs under <code>files/people/&lt;id&gt;/</code>. They cannot read yours. <code>memory/shared/</code> holds craft that every person's run may read. <a href="/sharing">Sharing</a> covers the rest.</p>

<h2 id="git">What is in git</h2>
<div class="wrap"><table>
<thead><tr><th>What</th><th>Where it is kept</th><th>Who sees it</th></tr></thead>
<tbody>
<tr><td><code>role/</code></td><td>Its own git repo. Every version that passes its checks is committed.</td><td>Only you, until you share it.</td></tr>
<tr><td><code>memory/</code>, <code>jobs.json</code>, private skills</td><td>Your agents repo, one private repo for all your agents.</td><td>Only you.</td></tr>
<tr><td><code>data/</code></td><td>Your agents repo, as text dumps. The database files themselves are ignored.</td><td>Only you.</td></tr>
<tr><td><code>files/</code></td><td>On disk only. Back it up with the rest of your Mac.</td><td>Only you.</td></tr>
<tr><td>Grants and keys</td><td>Grants in <code>~/.kith/secrets/</code>, keys in the macOS Keychain. Never in any repo.</td><td>Only you.</td></tr>
</tbody></table></div>
<p>Nothing leaves your Mac unless you say so. To keep a copy on GitHub, <code>kith backup run</code> makes a private repo for each piece that has none, then commits and pushes everything. <code>kith backup run --nightly</code> does that every night at 03:00.</p>

<h2 id="share">Share and install</h2>
<p>Sharing publishes the role. The person who installs it gets your skills and tools and starts with an empty memory.</p>
<h3>Publish a role</h3>
<pre><code>kith share reading                    <span class="c"># a private repo, &lt;you&gt;/kith-agent-reading</span>
kith share reading --public           <span class="c"># public, with its history</span>
kith share reading --public --fresh   <span class="c"># a public copy with no history, at &lt;you&gt;/kith-reading</span></code></pre>
<p>Sharing uses the GitHub CLI, <code>gh</code>. A public share first checks the role and its whole history for home paths, email addresses and the words you list in <code>~/.kith/private-words</code>, and refuses while it finds any. <code>--fresh</code> publishes the role as it is now, one commit per publish, while your own repo keeps every version and stays private. Each shared repo gets the GitHub topic <code>kith-agent</code>, which is how others find it.</p>
<h3>Install one</h3>
<pre><code>kith install owner/repo               <span class="c"># or a github.com URL</span>
kith install owner/repo --as books    <span class="c"># under another name</span>
kith install owner/repo --preview     <span class="c"># show what it would do, keep nothing</span>
kith market                           <span class="c"># agents people shared, most starred first</span>
kith search &lt;words&gt;                   <span class="c"># find agents on GitHub</span></code></pre>
<p>Any GitHub repo with an <code>agent.md</code> at its root installs. For one agent inside a bigger repo, give the folder: <code>owner/repo/tree/main/folder</code>.</p>
<p>Before an agent is kept, Kith shows what it would do on your Mac and asks: its suggested permission mode, what it may run without asking, its hooks, MCP servers, tools and skills, the services it asks for and the programs it needs. Nothing is granted by installing. The author's permission mode is a suggestion, and your answer is saved on your Mac, so a later update cannot change it.</p>
<pre><code>kith update books                     <span class="c"># pull the author's changes, fast-forward only</span>
kith update --all                     <span class="c"># Kith first, then every agent</span></code></pre>
<p>An update never overwrites your own edits. If you share your copy, the author's repo stays as its upstream, so updates keep coming.</p>

<h2 id="versions">Versions</h2>
<p>Every run is pinned to a snapshot of the role, so anyone can change an agent at any time, including the agent itself, without breaking a run in progress.</p>
<ol>
  <li>On each run Kith fingerprints the role. If nothing changed, the run uses the snapshot it already has.</li>
  <li>If it changed, Kith checks it: the frontmatter, the JSON files, the placeholders, that the skills named in <code>agent.md</code> exist, and that the services in <code>needs:</code> are defined.</li>
  <li>A version that passes is snapshotted and committed to the role's repo. A version that fails is never used. The last good one runs and the reason is printed.</li>
</ol>
<pre><code>kith history reading             <span class="c"># every committed version, newest first</span>
kith restore reading COMMIT      <span class="c"># bring one back, as a new commit</span>
kith versions reading            <span class="c"># snapshots on this Mac: current, good, pinned</span>
kith rollback reading            <span class="c"># pin the previous good version</span>
kith pin reading VERSION         <span class="c"># run this version until unpinned</span>
kith unpin reading               <span class="c"># follow the latest edits again</span></code></pre>
<p><code>history</code> and <code>restore</code> work on the role's git history, which is kept for good. A restore is a new commit, so another restore undoes it. <code>pin</code> and <code>rollback</code> choose among the snapshots on this Mac without changing any file. Old snapshots are cleared after two days unless they are pinned, good or in use.</p>
<p>For an edit that spans several files, create an empty <code>role/.editing</code> file first and delete it when you are done. Runs keep the last good version in the meantime. A marker older than an hour is ignored.</p>

<h2 id="engines">Engines</h2>
<p>An engine is the coding agent a run uses. Kith gives every engine the same prompt, memory, project and folders.</p>
<h3>Per agent</h3>
<p>Set <code>harness:</code> in <code>agent.md</code>, or let Kith write it:</p>
<pre><code>kith reading set harness codex
kith reading set harness codex --model MODEL   <span class="c"># with a model of that engine</span></code></pre>
<div class="wrap"><table>
<thead><tr><th>Value</th><th>Engine</th></tr></thead>
<tbody>
<tr><td><code>claude</code></td><td>Claude Code. The default when the field is absent.</td></tr>
<tr><td><code>codex</code></td><td>OpenAI Codex CLI</td></tr>
<tr><td><code>pi</code></td><td>pi</td></tr>
</tbody></table></div>
<p>The command also accepts <code>agy</code>, <code>hermes</code>, <code>qwen</code>, <code>dsh</code> and <code>opencode</code>. The app's engine picker lists Claude Code and Codex.</p>
<h3>Per run</h3>
<pre><code>kith reading -H codex "what did I finish?"
kith ask reading -H pi "what did I finish?"
kith schedule add reading "0 8 * * 5" "List what I finished" -H codex</code></pre>
<p><code>-m MODEL</code> and <code>-e EFFORT</code> override the model and effort the same way. A <code>model:</code> in <code>agent.md</code> names a model of the agent's own engine, so a run on another engine uses that engine's default unless you pass <code>-m</code>. A conversation you continue with <code>kith ask -c</code> stays on the engine it started on.</p>
<pre><code>kith engines                     <span class="c"># which engines are installed on this Mac</span></code></pre>
<h3>What only Claude Code runs have</h3>
<ul>
  <li>The agent's <code>settings.json</code>: its permission rules and hooks.</li>
  <li>Cost recorded per request. Other engines record one row per run.</li>
  <li>Runs for people you invite. They always use Claude Code, whatever <code>harness:</code> says.</li>
  <li>Calls from inside a Claude Code session as <code>kith:&lt;name&gt;</code>, which run in that session.</li>
</ul>
`,
  next: [
    ["/docs", "Guide", "Get started"],
    ["/sharing", "Next", "Share an agent with someone"],
  ],
};
