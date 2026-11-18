# AI coding agents

Hand a well-described issue to an agent and get a draft pull request back — without copying the
issue into another tool or linking the result by hand.

An **agent** is a colleague of its own kind. It has a name and an `@handle`, instructions, a
repository and base branch to work in, and an account of its own: add it to projects (or keep it
out of them) like anyone, and it sees exactly what that account may see. **Assign** an issue to
it, or **mention** it in a comment, and it starts. What it changes becomes a branch and a
**draft pull request** on your Git server, linked to the issue, with a comment saying what it
did; when the issue is a question that needs no change, the answer is the comment.

## What you need

1. **A model** on the server — the same connection AI assistance uses (`TASKCRAFT_AI_PROVIDER`,
   `TASKCRAFT_AI_API_KEY`, `TASKCRAFT_AI_MODEL`; see *AI assistance* in the README). An agent can
   name another model on that connection.
2. **A repository with API access** — *Project settings → Integrations → Git → Create
   branches…* (the token TaskCraft creates branches and pull requests with).
3. **A runner** — the program that does the work, on a machine of yours.

## Setting it up

*Administration → Agents*:

- **Add an agent**: name, handle, instructions ("we write Rust 2024; run `cargo test`; never
  touch `migrations/`"), the repository and base branch. It starts as **being tried out**: only
  workspace admins can start it. Try it on an issue, read what it does, then set *Who can start
  it* to everyone — or turn it off.
- **Add a runner**: you get a one-time registration token and the two commands:

  ```bash
  taskcraft-runner register --server https://tasks.example.com --token tcr_reg_…
  taskcraft-runner run
  ```

  `taskcraft-runner` is in the release archives; `deploy/runner/Dockerfile` builds a small image
  for it. Run it **in a container that holds nothing else** — it works on code a model wrote.
  With `run --once` it takes one run and exits, so a loop (or your orchestrator) gives every run
  a fresh container:

  ```bash
  while true; do
    docker run --rm -e TASKCRAFT_RUNNER_SERVER=https://tasks.example.com -e TASKCRAFT_RUNNER_TOKEN=tcr_… taskcraft-runner
  done
  ```

Give the agent's account the project roles it needs: *member* in the projects whose issues it
should take. It never starts on an issue it can't see.

## How a run goes

1. Assigning or mentioning queues a run — one at a time per agent and issue: a second mention
   while it's working doesn't start a second run. The issue shows *AI agents* with the run's
   state (queued, working, done, failed, stopped), a live log and **Stop**.
2. A runner claims it. It receives the issue (title, description, the comments everyone may
   read), the agent's instructions, and a token **for this run only**.
3. The runner downloads the repository at the base branch *through TaskCraft* and lets the agent
   work in the checkout. The built-in agent asks the model *through TaskCraft* and acts with a
   few tools: list, read, search, write, delete — and run a command, when the runner was started
   with `--allow-commands`. Paths never leave the checkout.
4. The runner hands the changed files back. TaskCraft creates the branch (named after the issue,
   `web-12-short-title`, or the next free name), one commit and the draft pull request, links
   them to the issue and lets the agent comment with its summary.

A run ends as failed when the model gives up, takes more steps than allowed (`--max-steps`,
default 40) or the runner disappears for half an hour. The pull request is a draft: a person
reviews and merges it.

## Who holds what

- The **Git token** and the **model key** stay on the server. The runner never sees them: it
  gets the source as an archive and asks the model through the server, which counts calls and
  tokens per run (at most 300 model calls a run).
- The **runner's token** only claims runs. The **run's token** only reads that run's source,
  asks the model, writes its log and hands in its result — and stops working the moment the run
  ends or is stopped.
- The **agent** acts as its own account: its comments and pull requests are its own in the
  history, runs are in the audit log (`agent.run.started`, `agent.run.finished`), and comments
  restricted to a group aren't sent to it.
- What it hands in is checked: paths inside the repository, never `.git`, up to 300 files and
  5 MB a run.

*Administration → Agents → Runs* lists what each agent did, with the tokens it took where the
provider reports them. The workspace switch *AI coding agents* turns the whole thing off.

## Your own agent instead of the built-in one

The built-in agent is deliberately simple. To use another one — a coding agent's command-line
tool, a script — start the runner with `--command`:

```bash
taskcraft-runner run --command 'my-agent --task "$TASKCRAFT_TASK_FILE" && my-agent --summary > "$TASKCRAFT_RESULT_FILE"'
```

The command runs in the checkout with `TASKCRAFT_TASK_FILE` (the issue and the instructions, as
Markdown), `TASKCRAFT_RESULT_FILE` (write the summary or the answer there) and
`TASKCRAFT_ISSUE` (the key). When it exits with 0, whatever it changed in the checkout is handed
in. Such a command needs its own access to a model; TaskCraft's stays on the server.
