import { Blocks, FolderOpen } from "lucide-react";
import {
  siBrave,
  siGithub,
  siGitlab,
  siGoogledrive,
  siHomeassistant,
  siLinear,
  siNotion,
  siObsidian,
  siPostgresql,
  siPuppeteer,
  siSentry,
  siSqlite,
  siTodoist
} from "simple-icons";
import styles from "./AppTile.module.css";

const BRAND_ICONS = [siGithub, siGitlab, siNotion, siGoogledrive, siPostgresql, siSqlite, siBrave, siPuppeteer, siSentry, siLinear, siTodoist, siObsidian, siHomeassistant];

/** Brand icon (or a generic one) of an app / MCP server, by name. */
export function AppTile({ name, small = false }: { name: string; small?: boolean }) {
  const normalized = name.toLowerCase().replace(/[^a-z0-9]/g, "");
  const brand = BRAND_ICONS.find((icon) => normalized === icon.slug || normalized === icon.title.toLowerCase().replace(/[^a-z0-9]/g, ""));
  return (
    <span className={[styles.tile, small ? styles.small : ""].filter(Boolean).join(" ")} aria-hidden="true">
      {brand ? <svg viewBox="0 0 24 24"><path d={brand.path} fill="currentColor" /></svg> : normalized === "filesystem" ? <FolderOpen /> : <Blocks />}
    </span>
  );
}
