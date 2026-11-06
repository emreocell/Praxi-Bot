export const PRODUCT_CONFIG = {
  appName: "Praxi-Bot",
  storeFileName: "settings.json"
} as const;

export interface AccentPreset {
  id: "blue" | "emerald" | "violet" | "amber" | "rose";
  value: string;
}

export const ACCENT_PRESETS: readonly AccentPreset[] = [
  { id: "blue", value: "#2563eb" },
  { id: "emerald", value: "#10b981" },
  { id: "violet", value: "#8b5cf6" },
  { id: "amber", value: "#f59e0b" },
  { id: "rose", value: "#e11d48" }
] as const;

export interface BlurPreset {
  id: "none" | "soft" | "medium" | "strong";
  value: string;
}

export const BLUR_PRESETS: readonly BlurPreset[] = [
  { id: "none", value: "0px" },
  { id: "soft", value: "6px" },
  { id: "medium", value: "14px" },
  { id: "strong", value: "24px" }
] as const;

export interface DimPreset {
  id: "none" | "light" | "medium" | "heavy";
  value: string;
}

export const DIM_PRESETS: readonly DimPreset[] = [
  { id: "none", value: "0" },
  { id: "light", value: "0.2" },
  { id: "medium", value: "0.45" },
  { id: "heavy", value: "0.7" }
] as const;
