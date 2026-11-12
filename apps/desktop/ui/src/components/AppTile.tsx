import { BrandIcon } from "./BrandIcon";

/** Brand icon (or a generic one) of an app / MCP server, by name. Kept for main's call sites; renders the ui-v2 BrandIcon. */
export function AppTile({ name, small = false }: { name: string; small?: boolean }) {
  return <BrandIcon name={name} small={small} />;
}
