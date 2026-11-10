import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
// MapLibre's own sheet first: the page's sheet restyles its controls.
import "maplibre-gl/dist/maplibre-gl.css";
import "./styles.css";
import { App } from "./App";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
