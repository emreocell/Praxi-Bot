import { expect, test } from "vitest";
import { buildThemeTokens } from "ui/surface-model";
import { contrastRatio, PALETTES, UI_STYLES } from "ui/theme-model";
import { WALLPAPERS } from "../src/lib/wallpaper";

test("wallpaper plates keep normal and secondary text readable in every theme and wallpaper", () => {
  for (const theme of PALETTES) {
    for (const style of UI_STYLES) {
      const tokens = buildThemeTokens(theme, theme.accent ?? theme.text, style, true);
      // Opaque fills make the guarantee independent of cloud position, dim and veil settings.
      // Check actual emitted tokens, rather than allowing the colour parser to default missing tokens.
      for (const palette of Object.values(WALLPAPERS)) {
        for (const pixel of palette.palette) {
          expect(pixel).toMatch(/^#[0-9a-f]{6}$/i);
          for (const surface of ["fill", "hover"]) {
            const bg = tokens[`--on-wallpaper-${surface}`];
            expect(bg, `${theme.id}/${style}/${surface}`).toMatch(/^#[0-9a-f]{6}$/i);
            for (const ink of ["text", "muted"]) {
              const fg = tokens[`--on-wallpaper-${ink}`];
              expect(fg).toMatch(/^#[0-9a-f]{6}$/i);
              expect(contrastRatio(fg, bg), `${theme.id}/${style}/${pixel}/${ink}`).toBeGreaterThanOrEqual(4.5);
            }
          }
          expect(contrastRatio(tokens["--on-wallpaper-border"], tokens["--on-wallpaper-fill"])).toBeGreaterThanOrEqual(3);
        }
      }
    }
  }
});
