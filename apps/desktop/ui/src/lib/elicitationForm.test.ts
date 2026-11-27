import { strict as assert } from "node:assert";
import { test } from "node:test";
import type { ElicitationField } from "../api/types";
import { readElicitation } from "./elicitationForm.ts";

function field(name: string, kind: ElicitationField["kind"]): ElicitationField {
  return {
    name, kind, label: name, description: null, required: false, blocked: false,
    min_length: null, max_length: null, format: null, minimum: null, maximum: null, options: []
  };
}

test("forms start empty and only serialize supported, explicitly supplied fields", () => {
  const fields = [
    field("note", "string"),
    field("days", "integer"),
    field("remind", "boolean"),
    { ...field("choice", "enum"), options: [{ value: "", label: "Empty" }] },
    { ...field("password", "string"), blocked: true },
    field("__proto__", "string")
  ];
  const data = new FormData();
  assert.deepEqual(readElicitation(fields, data), {});
  data.set("0", "hello");
  data.set("1", "0");
  data.set("2", "no");
  data.set("3", "0");
  data.set("4", "blocked");
  data.set("5", "own value");
  data.set("extra", "ignore");
  const result = readElicitation(fields, data);
  assert.deepEqual(JSON.parse(JSON.stringify(result)),
    JSON.parse('{"note":"hello","days":0,"remind":false,"choice":"","__proto__":"own value"}'));
  assert.equal(Object.getPrototypeOf(result), Object.prototype);
});
