"use strict";

const path = require("node:path");
const vscode = require("vscode");
const {
  diagnosticPosition,
  mutationDetailsText,
  mutationMessage,
  parseReportJson,
  survivedMutations,
} = require("./report");

const DIAGNOSTIC_SOURCE = "togi";
const DIFF_SCHEME = "togi-mutant-diff";

let diagnostics;
let reportWatcher;
let mutationsByDiagnosticKey = new Map();
let virtualDocuments = new Map();

function activate(context) {
  diagnostics = vscode.languages.createDiagnosticCollection(DIAGNOSTIC_SOURCE);
  context.subscriptions.push(diagnostics);

  context.subscriptions.push(
    vscode.workspace.registerTextDocumentContentProvider(DIFF_SCHEME, {
      provideTextDocumentContent(uri) {
        return (
          virtualDocuments.get(uri.toString()) ||
          "Mutation details are no longer available.\n"
        );
      },
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("togi.refreshReport", () =>
      refreshReport({ silent: false }),
    ),
    vscode.commands.registerCommand("togi.clearDiagnostics", clearDiagnostics),
    vscode.commands.registerCommand("togi.showMutationDiff", showMutationDiff),
    registerCodeActions(),
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (event.affectsConfiguration("togi.reportPath")) {
        configureReportWatcher(context);
        refreshReport({ silent: true });
      }
    }),
  );

  configureReportWatcher(context);
  refreshReport({ silent: true });
}

function deactivate() {
  if (reportWatcher) {
    reportWatcher.dispose();
    reportWatcher = undefined;
  }
  virtualDocuments = new Map();
  mutationsByDiagnosticKey = new Map();
}

function registerCodeActions() {
  return vscode.languages.registerCodeActionsProvider(
    { scheme: "file" },
    {
      provideCodeActions(document, _range, context) {
        return context.diagnostics
          .filter((diagnostic) => diagnostic.source === DIAGNOSTIC_SOURCE)
          .map((diagnostic) => {
            const mutation = mutationsByDiagnosticKey.get(
              diagnosticKey(document.uri, diagnostic.code),
            );
            if (!mutation) {
              return undefined;
            }

            const action = new vscode.CodeAction(
              "Show togi mutation diff",
              vscode.CodeActionKind.QuickFix,
            );
            action.command = {
              command: "togi.showMutationDiff",
              title: "Show togi mutation diff",
              arguments: [mutation],
            };
            action.diagnostics = [diagnostic];
            action.isPreferred = true;
            return action;
          })
          .filter(Boolean);
      },
    },
    {
      providedCodeActionKinds: [vscode.CodeActionKind.QuickFix],
    },
  );
}

async function refreshReport(options = {}) {
  const reportUri = resolveReportUri();
  if (!reportUri) {
    clearDiagnostics();
    if (!options.silent) {
      vscode.window.showWarningMessage("Open a workspace to load a togi report.");
    }
    return;
  }

  let report;
  try {
    const raw = await vscode.workspace.fs.readFile(reportUri);
    report = parseReportJson(Buffer.from(raw).toString("utf8"));
  } catch (error) {
    clearDiagnostics();
    if (!options.silent) {
      vscode.window.showWarningMessage(
        `Could not load ${reportUri.fsPath}: ${error.message}`,
      );
    }
    return;
  }

  const survived = survivedMutations(report);
  const grouped = new Map();
  mutationsByDiagnosticKey = new Map();

  for (const mutation of survived) {
    const uri = resolveMutationUri(mutation.file);
    if (!uri) {
      continue;
    }

    const position = diagnosticPosition(mutation);
    const range = new vscode.Range(
      position.line,
      position.character,
      position.line,
      position.character + 1,
    );
    const diagnostic = new vscode.Diagnostic(
      range,
      mutationMessage(mutation),
      vscode.DiagnosticSeverity.Warning,
    );
    diagnostic.source = DIAGNOSTIC_SOURCE;
    diagnostic.code =
      mutation.id === undefined
        ? `${mutation.file}:${mutation.line}:${mutation.column}`
        : String(mutation.id);

    const entries = grouped.get(uri.toString()) || { uri, diagnostics: [] };
    entries.diagnostics.push(diagnostic);
    grouped.set(uri.toString(), entries);
    mutationsByDiagnosticKey.set(diagnosticKey(uri, diagnostic.code), mutation);
  }

  diagnostics.clear();
  for (const entry of grouped.values()) {
    diagnostics.set(entry.uri, entry.diagnostics);
  }

  if (!options.silent) {
    vscode.window.setStatusBarMessage(
      `togi: loaded ${survived.length} survived mutation${
        survived.length === 1 ? "" : "s"
      }`,
      3000,
    );
  }
}

function clearDiagnostics() {
  if (diagnostics) {
    diagnostics.clear();
  }
  mutationsByDiagnosticKey = new Map();
}

async function showMutationDiff(mutation) {
  if (!mutation) {
    vscode.window.showWarningMessage("No togi mutation selected.");
    return;
  }

  const id = encodeURIComponent(String(mutation.id || "mutation"));
  const uri = vscode.Uri.parse(`${DIFF_SCHEME}:/${id}-${Date.now()}.diff`);
  virtualDocuments.set(uri.toString(), mutationDetailsText(mutation));

  const document = await vscode.workspace.openTextDocument(uri);
  try {
    await vscode.languages.setTextDocumentLanguage(document, "diff");
  } catch (_error) {
    // The virtual document is still useful even if VS Code cannot assign diff syntax.
  }
  await vscode.window.showTextDocument(document, { preview: true });
}

function configureReportWatcher(context) {
  if (reportWatcher) {
    reportWatcher.dispose();
    reportWatcher = undefined;
  }

  const folder = firstWorkspaceFolder();
  const configuredPath = configuredReportPath();
  if (!folder || path.isAbsolute(configuredPath)) {
    return;
  }

  reportWatcher = vscode.workspace.createFileSystemWatcher(
    new vscode.RelativePattern(folder, configuredPath),
  );
  context.subscriptions.push(reportWatcher);
  reportWatcher.onDidChange(() => refreshReport({ silent: true }));
  reportWatcher.onDidCreate(() => refreshReport({ silent: true }));
  reportWatcher.onDidDelete(clearDiagnostics);
}

function resolveReportUri() {
  const configuredPath = configuredReportPath();
  if (path.isAbsolute(configuredPath)) {
    return vscode.Uri.file(configuredPath);
  }

  const folder = firstWorkspaceFolder();
  if (!folder) {
    return undefined;
  }
  return joinWorkspacePath(folder.uri, configuredPath);
}

function resolveMutationUri(file) {
  if (path.isAbsolute(file)) {
    return vscode.Uri.file(file);
  }

  const folder = firstWorkspaceFolder();
  if (!folder) {
    return undefined;
  }
  return joinWorkspacePath(folder.uri, file);
}

function configuredReportPath() {
  return vscode.workspace
    .getConfiguration("togi")
    .get("reportPath", "togi-report.json");
}

function firstWorkspaceFolder() {
  return vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders[0];
}

function joinWorkspacePath(baseUri, filePath) {
  const segments = filePath.split(/[\\/]+/).filter(Boolean);
  return vscode.Uri.joinPath(baseUri, ...segments);
}

function diagnosticKey(uri, code) {
  return `${uri.toString()}#${String(code)}`;
}

module.exports = {
  activate,
  deactivate,
};
