// Sets the remembered theme before the first paint (index.html). Linked rather than inline so
// the release CSP needs no script hash. Kept in step with THEME_KEY in src/boot.ts.
try {
  var theme = localStorage.getItem("imperium.theme");
  if (theme === "light" || theme === "dark") document.documentElement.dataset.theme = theme;
} catch (error) {}
