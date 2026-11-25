# site-docs

MkDocs source for the published usage site (GitHub Pages).
`mkdocs.yml` at the repo root points here via `docs_dir`; the
existing `docs/` tree stays as developer notes and is not published.

## Layout

| Path | Contents |
|---|---|
| `index.md` | Landing page, rendered by `overrides/home.html` |
| `get-started/` | Quickstart and the server walkthrough |
| `concepts/` | Instances, schema, relationships, transactions, namespaces |
| `tutorials/` | Movies, TODO, and orders walkthroughs, each on its own page |
| `embedding/` | Per-language guides and the DQL/GraphQL reference |
| `guides/` | DQL, GraphQL, HTTP, admin, security, backups, tenancy, operations |
| `reference/` | Architecture, limitations, changelog |
| `assets/` | Design tokens, landing styles, logo, favicon, small scripts |

The landing page uses a custom template and design tokens; content pages use
Material for MkDocs with the same tokens applied.

## Build and check locally

```sh
pip install -r docs-requirements.txt
mkdocs build --strict
mkdocs serve  # preview at http://127.0.0.1:8000
```

Publishing runs in `.github/workflows/docs.yml` on pushes to
`master`: strict build, then `gh-deploy` to the `gh-pages` branch.
In the repository settings, set Pages to deploy from that branch.
