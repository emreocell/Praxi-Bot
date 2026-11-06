export interface MockBot {
  id: "atlas" | "nova" | "pulse";
  name: string;
  initials: string;
  roleKey: "bots.atlas.role" | "bots.nova.role" | "bots.pulse.role";
  color: string;
  avatarSrc?: string;
  isAnimated?: boolean;
}

const NOVA_SVG_DATA_URI =
  "data:image/svg+xml;utf8," +
  encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">' +
      '<rect width="64" height="64" rx="32" fill="#10b981"/>' +
      '<circle cx="32" cy="32" r="16" fill="#ecfdf5" fill-opacity="0.9"/>' +
      '<circle cx="32" cy="32" r="7" fill="#047857"/>' +
      "</svg>"
  );

// Minimal valid GIF89a image (purple tone) to demonstrate GIF avatar & prefers-reduced-motion handling
const PULSE_GIF_DATA_URI =
  "data:image/gif;base64,R0lGODlhAQABAIABAIv29v///yH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==";

export const MOCK_BOTS: readonly MockBot[] = [
  {
    id: "atlas",
    name: "Atlas",
    initials: "AT",
    roleKey: "bots.atlas.role",
    color: "#3b82f6"
  },
  {
    id: "nova",
    name: "Nova",
    initials: "NV",
    roleKey: "bots.nova.role",
    color: "#10b981",
    avatarSrc: NOVA_SVG_DATA_URI
  },
  {
    id: "pulse",
    name: "Pulse",
    initials: "PL",
    roleKey: "bots.pulse.role",
    color: "#8b5cf6",
    avatarSrc: PULSE_GIF_DATA_URI,
    isAnimated: true
  }
] as const;
