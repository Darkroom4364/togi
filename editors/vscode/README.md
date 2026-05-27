# togi VS Code extension

This extension reads the JSON report produced by:

```bash
togi check --format json > togi-report.json
```

It shows every survived mutant as a VS Code warning diagnostic. Use the quick
fix on a diagnostic to open the mutation details and diff.

## Configuration

- `togi.reportPath`: path to the JSON report, relative to the workspace root
  unless absolute. Defaults to `togi-report.json`.

## Development

```bash
npm run test
```

Open this folder in VS Code and run the extension host to try it locally.
