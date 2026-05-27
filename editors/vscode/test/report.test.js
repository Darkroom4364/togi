"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");
const {
  diagnosticPosition,
  mutationDetailsText,
  mutationMessage,
  parseReportJson,
  survivedMutations,
} = require("../report");

test("parseReportJson requires a mutations array", () => {
  assert.throws(
    () => parseReportJson("{}"),
    /expected a togi JSON report with a mutations array/,
  );
});

test("survivedMutations keeps only actionable survived mutants", () => {
  const report = parseReportJson(
    JSON.stringify({
      mutations: [
        {
          id: 1,
          file: "src/auth.rs",
          line: 47,
          column: 10,
          operator: "lt_to_lte",
          description: "changed < to <=",
          result: "survived",
        },
        {
          id: 2,
          file: "src/auth.rs",
          line: 48,
          result: "killed",
        },
        {
          id: 3,
          file: "",
          line: 1,
          result: "survived",
        },
      ],
    }),
  );

  assert.deepEqual(survivedMutations(report), [
    {
      id: 1,
      file: "src/auth.rs",
      line: 47,
      column: 10,
      operator: "lt_to_lte",
      description: "changed < to <=",
      original: undefined,
      replacement: undefined,
      diff: undefined,
    },
  ]);
});

test("diagnosticPosition converts one-based report coordinates to zero-based", () => {
  assert.deepEqual(diagnosticPosition({ line: 47, column: 10 }), {
    line: 46,
    character: 9,
  });
});

test("mutationMessage includes id, operator, and description", () => {
  assert.equal(
    mutationMessage({
      id: 9,
      operator: "eq_to_ne",
      description: "changed == to !=",
    }),
    "Survived mutation #9: eq_to_ne: changed == to !=",
  );
});

test("mutationDetailsText includes before-after values and diff fallback", () => {
  const text = mutationDetailsText({
    id: 9,
    file: "src/auth.rs",
    line: 47,
    operator: "eq_to_ne",
    description: "changed == to !=",
    original: "==",
    replacement: "!=",
  });

  assert.match(text, /src\/auth\.rs:47/);
  assert.match(text, /Original: ==/);
  assert.match(text, /Replacement: !=/);
  assert.match(text, /\(no diff was included/);
});
