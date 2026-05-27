"use strict";

function parseReportJson(content) {
  const report = JSON.parse(content);
  if (!report || typeof report !== "object" || !Array.isArray(report.mutations)) {
    throw new Error("expected a togi JSON report with a mutations array");
  }
  return report;
}

function survivedMutations(report) {
  if (!report || !Array.isArray(report.mutations)) {
    return [];
  }

  return report.mutations
    .filter((mutation) => mutation && mutation.result === "survived")
    .filter((mutation) => typeof mutation.file === "string" && mutation.file.length > 0)
    .filter((mutation) => Number.isInteger(mutation.line) && mutation.line > 0)
    .map((mutation) => ({
      id: Number.isInteger(mutation.id) ? mutation.id : undefined,
      file: mutation.file,
      line: mutation.line,
      column:
        Number.isInteger(mutation.column) && mutation.column > 0
          ? mutation.column
          : 1,
      operator:
        typeof mutation.operator === "string" && mutation.operator.length > 0
          ? mutation.operator
          : "mutation",
      description:
        typeof mutation.description === "string" ? mutation.description : "",
      original: optionalString(mutation.original),
      replacement: optionalString(mutation.replacement),
      diff: optionalString(mutation.diff),
    }));
}

function optionalString(value) {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function diagnosticPosition(mutation) {
  return {
    line: Math.max(0, mutation.line - 1),
    character: Math.max(0, (mutation.column || 1) - 1),
  };
}

function mutationMessage(mutation) {
  const id = mutation.id === undefined ? "" : ` #${mutation.id}`;
  const description = mutation.description ? `: ${mutation.description}` : "";
  return `Survived mutation${id}: ${mutation.operator}${description}`;
}

function mutationDetailsText(mutation) {
  const lines = [
    `Mutation #${mutation.id === undefined ? "unknown" : mutation.id}`,
    `${mutation.file}:${mutation.line}`,
    "",
    `Operator: ${mutation.operator}`,
  ];

  if (mutation.description) {
    lines.push(`Description: ${mutation.description}`);
  }

  if (mutation.original !== undefined || mutation.replacement !== undefined) {
    lines.push(
      "",
      `Original: ${mutation.original === undefined ? "(unknown)" : mutation.original}`,
      `Replacement: ${
        mutation.replacement === undefined ? "(unknown)" : mutation.replacement
      }`,
    );
  }

  lines.push("", "Diff:");
  if (mutation.diff) {
    lines.push(mutation.diff);
  } else {
    lines.push("(no diff was included in the JSON report)");
  }

  return `${lines.join("\n")}\n`;
}

module.exports = {
  diagnosticPosition,
  mutationDetailsText,
  mutationMessage,
  parseReportJson,
  survivedMutations,
};
