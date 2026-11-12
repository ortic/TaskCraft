# Website

The project page published on GitHub Pages by `.github/workflows/pages.yml` (Settings → Pages →
Source: GitHub Actions). Plain HTML and CSS: edit `index.html` and `style.css`, open
`index.html` in a browser to check.

## Screenshots

`screenshots/` comes from a fresh TaskCraft with the sample data:

```bash
# a fresh database and server (here on port 8790)
docker exec taskcraft-pg psql -U taskcraft -d postgres -c "CREATE DATABASE taskcraft_site"
TASKCRAFT_DATABASE_URL=postgres://taskcraft:taskcraft@127.0.0.1:55433/taskcraft_site \
  TASKCRAFT_BIND=127.0.0.1:8790 TASKCRAFT_BASE_URL=http://127.0.0.1:8790 \
  TASKCRAFT_DATA_DIR=/tmp/taskcraft-site cargo run -p taskcraft -- serve
```

Open http://127.0.0.1:8790, set up the workspace with the sample projects (owner "Alex Morgan",
alex@example.com), create a shared dashboard "Web team" (two counts, a "Highest priority" list
and two breakdowns), then:

```bash
cd e2e && node screenshots.mjs http://127.0.0.1:8790 alex@example.com <password> <dashboard id>
```
