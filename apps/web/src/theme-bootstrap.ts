// Restore the same persisted theme/sidebar before mounting the application.
try {
  const root = document.documentElement;
  const theme = localStorage.getItem("monica.theme.v1");
  if (theme === "light" || theme === "dark") root.dataset.theme = theme;
  if (localStorage.getItem("monica.sidebar.v1") === "collapsed") root.dataset.sidebar = "collapsed";
} catch { /* Private browsing can disable localStorage; system appearance still works. */ }
