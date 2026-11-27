import type { ElicitationContent, ElicitationField } from "../api/types";

/** HTML controls use indexes and can never set arbitrary object properties. */
export function readElicitation(
  fields: readonly ElicitationField[],
  data: FormData
): ElicitationContent {
  const entries = fields.flatMap((field, index): [string, string | number | boolean][] => {
    if (field.blocked) return [];
    const value = data.get(String(index));
    if (typeof value !== "string" || value === "") return [];
    switch (field.kind) {
      case "boolean":
        return value === "yes" || value === "no" ? [[field.name, value === "yes"]] : [];
      case "enum": {
        const option = field.options[Number(value)];
        return option ? [[field.name, option.value]] : [];
      }
      case "number":
      case "integer": {
        const number = Number(value);
        return Number.isFinite(number) ? [[field.name, number]] : [];
      }
      case "string":
        return [[field.name, value]];
    }
  });
  return Object.fromEntries(entries);
}
