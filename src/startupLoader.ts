/**
 * Dismiss the static loader declared in index.html once React has mounted.
 *
 * The loader must be dismissed by the root application rather than by a
 * screen that is conditionally rendered: first-run setup renders SetupScreen
 * and intentionally does not mount LoadingScreen.
 */
export const dismissStartupLoader = (
  rootDocument?: Pick<Document, "getElementById">,
): void => {
  const doc =
    rootDocument ??
    (typeof document !== "undefined" ? document : undefined);
  if (!doc) return;

  const startupLoader = doc.getElementById("app-startup-loader");
  if (!startupLoader) return;

  startupLoader.classList.add("loaded");
  // Keep the fade-out transition, then release the full-window overlay.
  setTimeout(() => startupLoader.remove(), 300);
};
