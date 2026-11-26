(function () {
  "use strict";

  var body = document.body;
  var syncTheme = function () {
    var dark = body.getAttribute("data-md-color-scheme") === "slate";
    document.documentElement.setAttribute("data-theme", dark ? "dark" : "light");
  };
  syncTheme();
  if (window.MutationObserver) {
    new MutationObserver(syncTheme).observe(body, {
      attributes: true,
      attributeFilter: ["data-md-color-scheme"],
    });
  }

  var figure = document.querySelector(".boundary");
  var buttons = document.querySelectorAll(".modes__btn");

  if (figure && buttons.length) {
    var requested = new URLSearchParams(window.location.search).get("mode");
    if (requested === "embedded" || requested === "standalone") {
      figure.setAttribute("data-mode", requested);
      buttons.forEach(function (button) {
        button.setAttribute(
          "aria-pressed",
          String(button.getAttribute("data-mode") === requested)
        );
      });
    }

    buttons.forEach(function (button) {
      button.addEventListener("click", function () {
        var mode = button.getAttribute("data-mode");
        if (!mode) return;
        figure.setAttribute("data-mode", mode);
        buttons.forEach(function (other) {
          other.setAttribute("aria-pressed", String(other === button));
        });
      });
    });
  }

  var copyButton = document.querySelector(".install__copy");
  if (copyButton) {
    copyButton.addEventListener("click", function () {
      var text = copyButton.getAttribute("data-copy") || "";
      var done = function (ok) {
        copyButton.textContent = ok ? "Copied" : "Copy manually";
        if (!ok) {
          var code = copyButton.closest(".install");
          code = code && code.querySelector("code");
          if (code && window.getSelection) {
            var range = document.createRange();
            range.selectNodeContents(code);
            var selection = window.getSelection();
            selection.removeAllRanges();
            selection.addRange(range);
          }
        }
        window.setTimeout(function () {
          copyButton.textContent = "Copy";
        }, 1800);
      };

      if (navigator.clipboard && window.isSecureContext) {
        navigator.clipboard.writeText(text).then(
          function () { done(true); },
          function () { done(false); }
        );
        return;
      }

      var field = document.createElement("textarea");
      field.value = text;
      field.setAttribute("readonly", "");
      field.style.position = "fixed";
      field.style.opacity = "0";
      document.body.appendChild(field);
      field.select();
      var ok = false;
      try {
        ok = document.execCommand("copy");
      } catch (error) {
        ok = false;
      }
      document.body.removeChild(field);
      done(ok);
    });
  }
})();
