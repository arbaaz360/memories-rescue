// Frontend placeholder for milestone M0. Plain TypeScript, no UI framework yet.
// All user-facing strings will move to an i18n catalogue before M3 (SRS C-09).

const APP_NAME = "Memories Rescue";

window.addEventListener("DOMContentLoaded", () => {
  document.title = APP_NAME;
  const status = document.querySelector<HTMLElement>("#status");
  if (status) {
    status.dataset.ready = "true";
  }
});
