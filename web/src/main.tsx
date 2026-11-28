import { createRoot } from "react-dom/client";
import { App } from "./app/App";
import { startWorker } from "./app/worker";
import "./features/settings/system/install-offer";

const root = document.getElementById("root");
if (!root) {
  throw new Error("Missing #root");
}

startWorker();
createRoot(root).render(<App />);
