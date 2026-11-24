# site-docs

MkDocs source for the published usage site (GitHub Pages).
`mkdocs.yml` at the repo root points here via `docs_dir`; the
existing `docs/` tree stays as developer notes and is not published.

Build and check locally:

```sh
pip install -r docs-requirements.txt
mkdocs build --strict
mkdocs serve  # preview at http://127.0.0.1:8000
```

Publishing runs in `.github/workflows/docs.yml` on pushes to
`master`: strict build, then `gh-deploy` to the `gh-pages` branch.
In the repository settings, set Pages to deploy from that branch.
