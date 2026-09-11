# Docs rendering interface fixture

From the Wake root, build the current CLI, then run:

```powershell
target/debug/wake.exe docs dev fixtures/docs-ui
target/debug/wake.exe docs dev fixtures/docs-ui/full
target/debug/wake.exe docs dev fixtures/docs-ui/default
```

Open `/handbook/`. The first configuration replaces Root, Header, SearchDialog and Demo;
the second replaces every public slot; the third exercises the unchanged default UI.
The same source content covers Context sharing, a legacy Preview wrapper, API data,
search, deployment paths, navigation and inline/portal style probes.

For crab-dev, copy the component registration and adapters into `.website/site`, declare
the directly imported Crab UI packages in `.website/package.json`, and set
`[docs].ui = "site/ui.tsx"`. Keep the existing content generator, navigation, workspace
mounts and Preview configuration. Start with the partial registration before replacing
Layout and Page. The adapter consumes public props; do not import Wake's generated modules.

Custom dialogs own keyboard/focus behavior. A custom Layout must render its children once;
a custom Page must render its children; a custom Demo must render `preview` once, moving
it into its fullscreen surface when needed. Context shared by Root and MDX lives in a
normal application module; the isolated demo iframe continues to use `docs.preview`.

Custom UI uses a complete development graph. This fixture does not change Components
workbench rendering or migrate the external crab-dev checkout.

See [VALIDATION.md](VALIDATION.md) for the implementation verification and validation scope.
