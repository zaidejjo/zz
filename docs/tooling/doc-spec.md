# `zz doc` — offline symbol lookup (spec, not yet shipped)

Planned thin wrapper over two files that already exist. No new backend needed.

## Lookup order

```bash
zz doc <symbol>   # e.g. zz doc std.http.route | zz doc "??" | zz doc chan.send
```

1. Exact match in `docs/stdlib/reference.md` (generated 691 entries) → print the
   module guide section (`docs/stdlib/<guide>#<fn>`).
2. Keyword match in `docs/_meta/search-index.json` (`keywords` + `excerpt`) →
   rank pages, print top 3 paths.
3. Language operators (`??`, `|>`, `?`, `..`) → `docs/language/operators.md` anchor.
4. Error messages → `docs/guides/error-index.md` row.

## Behavior

- Offline: reads the local `docs/` tree (or `~/.zz/docs/<tag>/` snapshot per
  [VERSIONING](../_meta/VERSIONING.md)). Never hits network.
- Output is the excerpt + file path, not a pager — pipeable to `less` / editors.
- Unknown symbol: list close matches via the same Levenshtein used for typo
  suggestions (`did you mean`).

## Until it ships

```bash
rg "std.http.route" docs/stdlib/
python3 -c "import json; print([p['path'] for p in json.load(open('docs/_meta/search-index.json'))['pages'] if 'chan' in p['keywords']])"
```

See [Conventions](../CONVENTIONS.md) for link rules the implementation must follow.
