// A link to a question of the FAQ opens it.
(() => {
  "use strict";
  function openTarget() {
    const el = location.hash && document.getElementById(location.hash.slice(1));
    if (el && el.tagName === "DETAILS") el.open = true;
  }
  addEventListener("hashchange", openTarget);
  openTarget();
})();
