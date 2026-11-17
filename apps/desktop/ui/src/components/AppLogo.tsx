import { Blocks } from "lucide-react";
import { useEffect, useState } from "react";
import { brandIcon } from "../lib/brandIcons";
import { useDaemon } from "../state/DaemonContext";
import { BrandMark } from "./BrandMark";
import styles from "./AppLogo.module.css";

/** Logos already asked for, by app slug: one request per app per session. */
const fetched = new Map<string, Promise<string | null>>();

/**
 * An app's logo: its bundled brand mark when Praxi ships one; otherwise the logo the daemon
 * fetched, validated and cached (loaded with the app's own credentials, shown from a blob: the
 * window's CSP allows no remote image); otherwise a neutral glyph. Never a letter monogram.
 */
export function AppLogo({ name, slug, size = 26 }: { name: string; slug?: string; size?: number }) {
  const { client } = useDaemon();
  const known = brandIcon(name).known;
  const [src, setSrc] = useState<string | null>(null);
  useEffect(() => {
    if (known || !slug || !client) {
      return;
    }
    let live = true;
    let pending = fetched.get(slug);
    if (!pending) {
      pending = client
        .composioLogo(slug)
        .then((blob) => URL.createObjectURL(blob))
        .catch(() => null);
      fetched.set(slug, pending);
    }
    void pending.then((url) => {
      if (live) {
        setSrc(url);
      }
    });
    return () => {
      live = false;
    };
  }, [known, slug, client]);
  if (known) {
    return <BrandMark name={name} size={size} />;
  }
  if (src) {
    return <img className={styles.img} src={src} alt="" width={size} height={size} draggable={false} data-testid="app-logo-img" />;
  }
  return (
    <span className={styles.glyph} style={{ width: size, height: size }} aria-hidden="true" data-testid="app-logo-glyph">
      <Blocks />
    </span>
  );
}
