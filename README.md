# TaskCraft

Self-hosted issue and project management with a **Rust backend**: boards, backlogs and sprints,
an issue query language, reports from the issues' history, automation, and documentation next to
the work through [WikiCraft](https://github.com/ortic/WikiCraft).

Status: in development. The backend API is in place; the web app follows.

## Development

Requirements: Rust ≥ 1.94, PostgreSQL ≥ 15 (or Docker).

```bash
docker run -d --name taskcraft-pg -p 127.0.0.1:55433:5432 \
  -e POSTGRES_USER=taskcraft -e POSTGRES_PASSWORD=taskcraft -e POSTGRES_DB=taskcraft postgres:17-alpine
cargo run -p taskcraft -- serve          # http://127.0.0.1:8788, migrations run on start
cargo test --workspace
```
