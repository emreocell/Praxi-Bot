import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "ui/tokens.css";
import { App } from "./App";
import "./global.css";
import "./i18n";

const container = document.getElementById("root");
if (!container) {
  throw new Error("Root container #root not found");
}

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>
);
