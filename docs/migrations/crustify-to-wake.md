# Migrating from Crustify to Wake

Crustify is scheduled for retirement after Wake 0.1.0 passes every registry
installation gate. Wake does not load
`.crustify.ts`, execute Crustify mods, or expose the previous JavaScript API.

## Commands

| Crustify | Wake |
| --- | --- |
| `crustify run-task app:build` | `wake build` |
| `crustify run-task app:dev` | `wake dev` |

Install Wake with:

```sh
npm uninstall @crab-dev/crustify
npm install --save-dev @crab-dev/wake
```

Move declarative project settings into `wake.config.toml`. Executable
configuration hooks must be replaced with Wake's supported declarative
settings or explicit scripts around the Wake API.

The old npm versions remain available for reproducible installs. The package is
deprecated only after every supported Wake platform passes registry smoke tests; it is
never unpublished by this migration.
