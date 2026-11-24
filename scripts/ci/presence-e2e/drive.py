"""The presence check's end-to-end scenarios, run by run.sh inside a session bus on an X display.

Starts the release daemon on a scratch home, adds a folder marketplace, and drives the release
desktop app through tauri-driver (WebDriver): each scenario calls the app's own `plugin_approve`
command from its main window, answers the native dialog through the accessibility bus (AT-SPI),
and answers polkit's prompt through pkttyagent in a terminal. Every scenario checks the command's
answer and what `imperium plugin list` then shows. Prints one "scenario ...: passed" line each, and
exits non-zero on the first failure. Standard library and PyGObject only.
"""

import http.client
import json
import os
import pty
import shutil
import signal
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

ROOT = Path(os.environ["E2E_ROOT"])
LOGS = Path(os.environ["E2E_LOGS"])
PASSWORD = os.environ["E2E_PASSWORD"]
ACTION_FILE = os.environ["E2E_ACTION_FILE"]
INSTALLED_ACTION = os.environ["E2E_INSTALLED_ACTION"]
RELEASE = ROOT / "target" / "release"
DAEMON = RELEASE / "imperium-core"
CLI = RELEASE / "imperium"
APP = RELEASE / "imperium-desktop"
HOME = Path(os.environ.get("RUNNER_TEMP", "/tmp")) / "presence-e2e-home"
MARKET = LOGS / "market"
MARKET_NAME = "e2e"
# The environment the app selects on a fresh home: the daemon's default one.
ENV_NAME = "Home"
DRIVER = "http://127.0.0.1:4444"
DIALOG_TITLE = "Approve plugin"


def log(message):
    print(message, flush=True)


class Failed(Exception):
    pass


def check(condition, message):
    if not condition:
        raise Failed(message)


# ---- The scratch home and marketplace ----


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def hooks(commands):
    entries = [{"type": "command", "command": command} for command in commands]
    return json.dumps({"hooks": {"PreToolUse": [{"matcher": "*", "hooks": entries}]}})


def make_market():
    """A folder marketplace: one-hook plugins for each scenario, and `big`, whose summary takes
    several dialog pages (about 90 hooks and one 300-character command)."""
    plugins = ["alpha", "gamma", "delta", "epsilon", "zeta", "uix"]
    entries = [{"name": name, "source": f"./{name}"} for name in plugins + ["big"]]
    write(
        MARKET / ".claude-plugin/marketplace.json",
        json.dumps({"name": MARKET_NAME, "owner": {"name": "CI"}, "plugins": entries}),
    )
    for name in plugins:
        write(MARKET / name / ".claude-plugin/plugin.json", json.dumps({"name": name}))
        write(MARKET / name / "hooks/hooks.json", hooks([f"echo {name}"]))
    set_uix("echo uix-1")
    write(MARKET / "big/.claude-plugin/plugin.json", json.dumps({"name": "big"}))
    commands = [f"echo hook-{index}" for index in range(90)]
    commands.append("echo " + "x" * 295)
    write(MARKET / "big/hooks/hooks.json", hooks(commands))


def set_uix(command):
    """The Plugins window scenario's plugin: one hook, whose command a scenario changes to make
    an update."""
    write(MARKET / "uix/hooks/hooks.json", hooks([command]))


# ---- The daemon and the command line ----


def env():
    values = dict(os.environ)
    values["IMPERIUM_HOME"] = str(HOME)
    return values


def cli(*args, timeout=180):
    done = subprocess.run(
        [str(CLI), *args], env=env(), capture_output=True, text=True, timeout=timeout
    )
    with open(LOGS / "cli.log", "a") as out:
        out.write(f"$ imperium {' '.join(args)}\n{done.stdout}{done.stderr}\n")
    if done.returncode != 0:
        raise Failed(f"imperium {' '.join(args)} failed: {done.stdout}{done.stderr}")
    return done.stdout


def start_daemon():
    write(HOME / "config/config.toml", "[daemon]\nsocket = true\n")
    out = open(LOGS / "daemon.log", "w")
    daemon_env = env()
    daemon_env["IMPERIUM_LOG"] = "info,imperium_core=debug"
    process = subprocess.Popen(
        [str(DAEMON)], env=daemon_env, stdout=out, stderr=subprocess.STDOUT
    )
    endpoint = HOME / "data/daemon.json"
    for _ in range(300):
        if endpoint.exists():
            try:
                found = json.loads(endpoint.read_text())
            except ValueError:
                found = None
            if found and found.get("socket") and found.get("pid") == process.pid:
                log(f"daemon {process.pid} serves {found['socket']}")
                return process
        if process.poll() is not None:
            raise Failed(f"the daemon exited with {process.returncode}: see daemon.log")
        time.sleep(0.2)
    raise Failed("the daemon wrote no endpoint with a local socket")


def plugin_list(environment=None):
    """`imperium plugin list` (for an environment, when given) as {name: (state, approval)}."""
    listed = {}
    current = None
    args = ["plugin", "list"] + (["--env", environment] if environment else [])
    for line in cli(*args).splitlines():
        if line and not line.startswith(" ") and ": " in line:
            name = line.split(" ", 1)[0].split(":", 1)[0]
            state = line.rsplit(": ", 1)[1]
            current = name
            listed[name] = [state, None]
        elif current and line.strip().startswith("origin:") and "approval: " in line:
            listed[current][1] = line.split("approval: ", 1)[1].strip()
    return {name: tuple(value) for name, value in listed.items()}


# ---- polkit's authentication agent ----


class Agent:
    """pkttyagent registered for the daemon's process, in a terminal of its own, answering each
    password prompt with `password`."""

    def __init__(self, pid, password, name):
        self.password = password
        self.output = b""
        self.answered = 0
        self.log = open(LOGS / f"pkttyagent-{name}.log", "wb")
        child, fd = pty.fork()
        if child == 0:
            os.execvp("pkttyagent", ["pkttyagent", "--process", str(pid)])
        self.child, self.fd = child, fd
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        time.sleep(2)
        check(self.alive(), f"pkttyagent exited at start: {self.text()}")

    def alive(self):
        return os.waitpid(self.child, os.WNOHANG) == (0, 0)

    def _read(self):
        while True:
            try:
                data = os.read(self.fd, 4096)
            except OSError:
                return
            if not data:
                return
            self.output += data
            self.log.write(data)
            self.log.flush()
            if self.output.count(b"Password:") > self.answered:
                self.answered += 1
                time.sleep(0.3)
                os.write(self.fd, self.password.encode() + b"\n")

    def text(self):
        return self.output.decode(errors="replace")

    def stop(self):
        try:
            os.kill(self.child, signal.SIGTERM)
            os.waitpid(self.child, 0)
        except (ProcessLookupError, ChildProcessError):
            pass
        try:
            os.close(self.fd)
        except OSError:
            pass


# ---- The app, through WebDriver ----


def webdriver(method, path, body=None, timeout=60):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(
        DRIVER + path, data=data, method=method, headers={"Content-Type": "application/json"}
    )
    # tauri-driver reuses its connection to WebKitWebDriver, which drops idle ones: the first
    # request after a quiet spell can be refused before it is read, and asking again works. A
    # long `execute/async` (an approval) is never repeated.
    attempts = 1 if "execute/async" in path else 4
    for attempt in range(attempts):
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                return json.load(response)["value"]
        except urllib.error.HTTPError as error:
            raise Failed(f"WebDriver {method} {path}: {error.code} {error.read().decode()}") from None
        except (http.client.RemoteDisconnected, ConnectionResetError):
            if attempt + 1 == attempts:
                raise
            time.sleep(1)


class Driver:
    """tauri-driver (and WebKitWebDriver under it), with `extra` added to its environment, which
    the app it launches inherits."""

    def __init__(self, name, extra=None):
        driver_env = env()
        driver_env.update(extra or {})
        self.log = open(LOGS / f"tauri-driver-{name}.log", "w")
        self.process = subprocess.Popen(
            ["tauri-driver", "--port", "4444"],
            env=driver_env,
            stdout=self.log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        for _ in range(100):
            try:
                webdriver("GET", "/status", timeout=2)
                break
            except (OSError, Failed):
                time.sleep(0.2)
        else:
            raise Failed("tauri-driver did not start")
        capabilities = {
            "capabilities": {
                "alwaysMatch": {"browserName": "wry", "tauri:options": {"application": str(APP)}}
            }
        }
        self.session = webdriver("POST", "/session", capabilities, timeout=120)["sessionId"]
        webdriver("POST", f"/session/{self.session}/timeouts", {"script": 420_000})
        for _ in range(240):
            try:
                kind = webdriver(
                    "POST",
                    f"/session/{self.session}/execute/sync",
                    {"script": "return typeof window.__TAURI_INTERNALS__", "args": []},
                )
            except OSError:
                # The page is still loading (or the driver is busy): ask again.
                time.sleep(0.5)
                continue
            if kind == "object":
                break
            time.sleep(0.5)
        else:
            raise Failed("the app's window never exposed its command bridge")
        log(f"app session {self.session} is up ({name})")

    def start_approve(self, name, action):
        """Calls `plugin_approve` from the main window in the background; `wait()` gives the
        answer."""
        script = (
            "const [name, action, done] = arguments;"
            "window.__TAURI_INTERNALS__.invoke('plugin_approve', {name, action})"
            ".then(r => done({ok: r}), e => done({error: String(e)}));"
        )
        result = {}

        def call():
            try:
                result["value"] = webdriver(
                    "POST",
                    f"/session/{self.session}/execute/async",
                    {"script": script, "args": [name, action]},
                    timeout=450,
                )
            except Exception as error:  # reported by wait()
                result["error"] = error

        thread = threading.Thread(target=call, daemon=True)
        thread.start()

        def wait(limit=400):
            thread.join(limit)
            check(not thread.is_alive(), f"plugin_approve({name}) did not answer")
            if "error" in result:
                raise result["error"]
            value = result["value"]
            check("ok" in value, f"plugin_approve({name}) failed: {value}")
            log(f"plugin_approve({name!r}, {action!r}) answered {value['ok']}")
            return value["ok"]

        wait.thread = thread
        wait.result = result
        return wait

    def js(self, script, *args):
        return webdriver(
            "POST", f"/session/{self.session}/execute/sync", {"script": script, "args": list(args)}
        )

    def ui_click(self, selector, text=None, limit=30):
        """Clicks the first visible element matching `selector` whose trimmed text is `text` (any
        text when None), waiting for it to appear."""
        script = (
            "const [sel, text] = arguments;"
            "const el = [...document.querySelectorAll(sel)]"
            ".filter(e => e.getClientRects().length > 0)"
            ".find(e => text === null || e.textContent.trim() === text);"
            "if (!el || el.disabled) return false; el.click(); return true;"
        )
        deadline = time.time() + limit
        while time.time() < deadline:
            if self.js(script, selector, text):
                return
            time.sleep(0.5)
        describe_screen(f"no-element-{int(time.time())}")
        raise Failed(f"no enabled element {selector!r} with text {text!r} to click: {self.page()}")

    def ui_text(self, selector):
        return self.js(
            "const el = document.querySelector(arguments[0]); return el ? el.textContent : null;",
            selector,
        )

    def ui_wait(self, selector, text=None, limit=30):
        """Waits for an element matching `selector` whose text contains `text`; its text."""
        deadline = time.time() + limit
        while time.time() < deadline:
            found = self.js(
                "const [sel, text] = arguments;"
                "const el = [...document.querySelectorAll(sel)]"
                ".find(e => text === null || e.textContent.includes(text));"
                "return el ? el.textContent : null;",
                selector,
                text,
            )
            if found is not None:
                return found
            time.sleep(0.5)
        raise Failed(f"no element {selector!r} containing {text!r}: {self.page()}")

    def page(self):
        """The visible text of the Plugins window, for a failure message."""
        return self.js(
            "const d = document.querySelector('[role=dialog]'); return d ? d.innerText.slice(0, 1500) : document.body.innerText.slice(0, 500);"
        )

    def close(self):
        try:
            webdriver("DELETE", f"/session/{self.session}", timeout=30)
        except (OSError, Failed) as error:
            log(f"closing the session: {error}")
        os.killpg(self.process.pid, signal.SIGTERM)
        self.process.wait(timeout=30)
        # Let the port and the app's window go before the next driver starts on the same port.
        time.sleep(3)


# ---- The native dialog, through AT-SPI ----


def walk(node, depth=0):
    yield node
    if depth >= 15:
        return
    for index in range(node.get_child_count()):
        child = node.get_child_at_index(index)
        if child is not None:
            yield from walk(child, depth + 1)


def dialogs():
    """The app's open approval dialogs, as (title, node). GTK names a message dialog's accessible
    after its kind ("Warning"), so a dialog is an alert or dialog holding a label that starts with
    "Approve plugin": the primary text, which is the page's title."""
    found = []
    desktop = Atspi.get_desktop(0)
    desktop.clear_cache()
    for index in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(index)
        if app is None:
            continue
        app.clear_cache()
        for position in range(app.get_child_count()):
            window = app.get_child_at_index(position)
            if window is None or window.get_role() not in (Atspi.Role.ALERT, Atspi.Role.DIALOG):
                continue
            for node in walk(window):
                name = node.get_name() or ""
                if node.get_role() == Atspi.Role.LABEL and name.startswith(DIALOG_TITLE):
                    found.append((name, window))
                    break
    return found


def describe_screen(name):
    """What is on the display, for a failure: the accessibility tree's top levels, the X window
    tree and a screenshot."""
    lines = []
    desktop = Atspi.get_desktop(0)
    desktop.clear_cache()
    for index in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(index)
        if app is None:
            continue
        lines.append(f"app {app.get_name()!r}")
        for position in range(app.get_child_count()):
            window = app.get_child_at_index(position)
            if window is not None:
                lines.append(
                    f"  {window.get_role_name()} {window.get_name()!r} "
                    f"children={window.get_child_count()}"
                )
    (LOGS / f"atspi-{name}.txt").write_text(chr(10).join(lines) + chr(10))
    log("accessibility tree:" + chr(10) + chr(10).join(lines))
    with open(LOGS / f"xwininfo-{name}.txt", "w") as out:
        subprocess.run(["xwininfo", "-root", "-tree"], stdout=out, stderr=subprocess.STDOUT)
    subprocess.run(["import", "-window", "root", str(LOGS / f"screen-{name}.png")], check=False)


def next_dialog(seen, call, limit=60):
    """Waits for a dialog whose title was not `seen` yet; its title, text and buttons. Fails at
    once when the `plugin_approve` call answered instead."""
    deadline = time.time() + limit
    while time.time() < deadline:
        if not call.thread.is_alive():
            raise Failed(f"plugin_approve answered before a dialog was seen: {call.result}")
        for title, dialog in dialogs():
            if title in seen:
                continue
            nodes = list(walk(dialog))
            buttons = {
                node.get_name(): node
                for node in nodes
                if node.get_role() == Atspi.Role.PUSH_BUTTON and node.get_name()
            }
            text = "\n".join(
                node.get_name() or ""
                for node in nodes
                if node.get_role() == Atspi.Role.LABEL
            )
            if buttons:
                return title, text, buttons
        time.sleep(0.5)
    describe_screen(f"no-dialog-{len(seen)}")
    raise Failed(f"no new approval dialog appeared (seen: {sorted(seen)})")


def press(button):
    action = button.get_action_iface()
    check(action is not None, f"the {button.get_name()} button offers no action")
    action.do_action(0)


def answer_pages(answers, call):
    """Answers successive dialog pages with `answers` ("Next", "Approve" or "Cancel"); the pages'
    titles and texts."""
    seen = set()
    pages = []
    for answer in answers:
        title, text, buttons = next_dialog(seen, call)
        seen.add(title)
        log(f"dialog page {title!r} offers {sorted(buttons)}; pressing {answer}")
        pages.append((title, text, sorted(buttons)))
        check(answer in buttons, f"page {title!r} has no {answer} button: {sorted(buttons)}")
        press(buttons[answer])
    return pages


def no_dialog_appears(seconds=5):
    deadline = time.time() + seconds
    while time.time() < deadline:
        check(not dialogs(), "a dialog appeared where none should")
        time.sleep(0.5)


# ---- Scenarios ----


def set_action_file(installed):
    if installed:
        subprocess.run(["sudo", "install", "-m", "0644", ACTION_FILE, INSTALLED_ACTION], check=True)
    else:
        subprocess.run(["sudo", "rm", "-f", INSTALLED_ACTION], check=True)
    subprocess.run(["sudo", "systemctl", "restart", "polkit"], check=True)
    for _ in range(30):
        known = (
            subprocess.run(
                ["pkaction", "--action-id", "org.imperium.plugins.approve"],
                capture_output=True,
            ).returncode
            == 0
        )
        if known == installed:
            return
        time.sleep(1)
    raise Failed(f"polkit did not {'load' if installed else 'drop'} the action")


class Pending:
    """Stands for a call that is under way in the page, for `next_dialog`, which stops waiting when
    the call it is given has answered; the page's own answer shows in its notice instead."""

    class thread:
        @staticmethod
        def is_alive():
            return True

    result = {}


def scenario(name, run):
    log(f"--- scenario {name}")
    run()
    log(f"scenario {name}: passed")


def main():
    make_market()
    daemon = start_daemon()
    agent = None
    driver = None
    try:
        cli("plugin", "marketplace", "add", "--", str(MARKET))
        driver = Driver("main")

        def approve_counted():
            nonlocal agent
            agent = Agent(daemon.pid, PASSWORD, "counted")
            wait = driver.start_approve(f"alpha@{MARKET_NAME}", "install")
            pages = answer_pages(["Approve"], wait)
            title, text, buttons = pages[0]
            check(title == DIALOG_TITLE, f"one page expected, got {title!r}")
            check(f"alpha@{MARKET_NAME}" in text, f"the dialog does not name the plugin: {text}")
            check("echo alpha" in text, f"the dialog does not show the hook: {text}")
            check("Content hash" in text, f"the dialog does not show the hash: {text}")
            answer = wait()
            check(answer["approved"] and answer["kind"] == "counted", f"{answer}")
            check(answer["installed"], f"{answer}")
            prompt = agent.text()
            check("org.imperium.plugins.approve" in prompt, f"polkit asked nothing: {prompt}")
            check(
                "approve the Imperium plugin" in prompt,
                f"polkit's prompt is not the action's message: {prompt}",
            )
            listed = plugin_list()
            check(listed.get("alpha") == ("on", "counted"), f"plugin list: {listed}")
            agent.stop()
            agent = None

        def cancel_on_page_two():
            wait = driver.start_approve(f"big@{MARKET_NAME}", "install")
            pages = answer_pages(["Next", "Cancel"], wait)
            first, second = pages[0][0], pages[1][0]
            check(" - page 1 of " in first, f"first page title {first!r}")
            check(" - page 2 of " in second, f"second page title {second!r}")
            count = int(first.rsplit(" ", 1)[1])
            check(count >= 3, f"the big plugin should take several pages, took {count}")
            check("Approve" not in pages[0][2], f"page 1 offers Approve: {pages[0][2]}")
            check(f"big@{MARKET_NAME}" in pages[1][1], "page 2 does not repeat the id")
            check("Content hash" in pages[1][1], "page 2 does not repeat the hash")
            answer = wait()
            check(answer["cancelled"] and not answer["installed"], f"{answer}")
            check(not answer["approved"], f"{answer}")
            check("big" not in plugin_list(), "the cancelled plugin was installed")

        def refused():
            nonlocal agent
            agent = Agent(daemon.pid, "not-the-password", "refused")
            wait = driver.start_approve(f"gamma@{MARKET_NAME}", "install")
            answer_pages(["Approve"], wait)
            answer = wait()
            check(not answer["approved"] and answer["installed"], f"{answer}")
            check("presence check did not pass" in (answer["reason"] or ""), f"{answer}")
            check("org.imperium.plugins.approve" in agent.text(), "polkit asked nothing")
            listed = plugin_list()
            check(listed.get("gamma") == ("withheld", "not approved"), f"plugin list: {listed}")
            agent.stop()
            agent = None

        def no_agent():
            wait = driver.start_approve(f"delta@{MARKET_NAME}", "install")
            answer_pages(["Approve"], wait)
            answer = wait()
            check(answer["approved"] and answer["kind"] == "dialog-only", f"{answer}")
            listed = plugin_list()
            check(listed.get("delta") == ("on", "dialog-only"), f"plugin list: {listed}")

        def no_action_file():
            nonlocal agent
            set_action_file(False)
            # An agent is running and would answer: only the missing action file makes it
            # dialog-only.
            agent = Agent(daemon.pid, PASSWORD, "no-action-file")
            wait = driver.start_approve(f"epsilon@{MARKET_NAME}", "install")
            answer_pages(["Approve"], wait)
            answer = wait()
            check(answer["approved"] and answer["kind"] == "dialog-only", f"{answer}")
            check("org.imperium.plugins.approve" not in agent.text(), "polkit was asked")
            listed = plugin_list()
            check(listed.get("epsilon") == ("on", "dialog-only"), f"plugin list: {listed}")
            agent.stop()
            agent = None
            set_action_file(True)

        def plugins_window():
            """The Plugins window's trust UI, end to end: install, approve (polkit), enable and
            disable for one environment and for the user, update (withheld until approved
            again), uninstall and undo. Each step is checked through `imperium plugin list`."""
            nonlocal agent
            env_name = ENV_NAME
            agent = Agent(daemon.pid, PASSWORD, "window")
            # The first-run tour starts by itself and closes the windows it finds open; skip it.
            try:
                driver.ui_click("button", "Skip tour", limit=20)
            except Failed:
                log("no first-run tour was showing")
            driver.ui_click('button[aria-label="Plugins"]')
            driver.ui_wait("[role=dialog]", "Plugins")

            # Install: Browse, the plugin's detail, the trust view with what it runs.
            driver.ui_click('button[data-plugin-id="uix@' + MARKET_NAME + '"]')
            driver.ui_click("button", "Install\u2026")
            trust = driver.ui_wait(".trust-view", "echo uix-1")
            check("Hooks (1)" in trust, f"the trust view does not count the hook: {trust}")
            check("Content hash" in trust, f"the trust view shows no hash: {trust}")
            check("uix@" + MARKET_NAME in trust, f"the trust view does not name the plugin: {trust}")

            # Approve and install: the native dialog, then polkit.
            driver.ui_click("button", "Approve and install")
            answer_pages(["Approve"], Pending)
            told = driver.ui_wait(".notice-box", "Approved", limit=60)
            check("device" in told, f"the window did not report a counted approval: {told}")
            check(plugin_list().get("uix") == ("on", "counted"), f"plugin list: {plugin_list()}")
            check("org.imperium.plugins.approve" in agent.text(), "polkit asked nothing")
            row = driver.ui_wait('[data-plugin="uix"]', "counted")
            check("Withheld" not in row, f"the installed row says withheld: {row}")

            # Disable and enable for one environment, then for the user.
            driver.ui_click("button", "In " + env_name)
            label = f"uix on in {env_name}"
            driver.ui_click(f'[role=switch][aria-label="{label}"]')
            for _ in range(20):
                if plugin_list(env_name).get("uix", ("", ""))[0] == "off":
                    break
                time.sleep(0.5)
            check(plugin_list(env_name).get("uix", ("", ""))[0] == "off", "off for the environment")
            check(plugin_list().get("uix", ("", ""))[0] == "on", "the user's view should stay on")
            driver.ui_click(f'[role=switch][aria-label="{label}"]')
            for _ in range(20):
                if plugin_list(env_name).get("uix", ("", ""))[0] == "on":
                    break
                time.sleep(0.5)
            check(plugin_list(env_name).get("uix", ("", ""))[0] == "on", "on again for the environment")
            driver.ui_click("button", "For you")
            driver.ui_click('[role=switch][aria-label="uix on for you"]')
            for _ in range(20):
                if plugin_list().get("uix", ("", ""))[0] == "off":
                    break
                time.sleep(0.5)
            check(plugin_list().get("uix", ("", ""))[0] == "off", "off for the user")
            driver.ui_click('[role=switch][aria-label="uix on for you"]')
            for _ in range(20):
                if plugin_list().get("uix") == ("on", "counted"):
                    break
                time.sleep(0.5)
            check(plugin_list().get("uix") == ("on", "counted"), f"on again: {plugin_list()}")

            # Update: the marketplace changes the hook; the trust view shows the new command, and
            # updating without approving leaves it withheld until it is approved again.
            set_uix("echo uix-2")
            cli("plugin", "marketplace", "refresh", MARKET_NAME)
            driver.ui_click('button[aria-label="Check uix for an update"]')
            trust = driver.ui_wait(".trust-view", "echo uix-2")
            check("Update" in trust, f"not an update view: {trust}")
            driver.ui_click("button", "Update without approving")
            driver.ui_wait(".notice-box", "withheld")
            listed = plugin_list()
            check(listed.get("uix") == ("withheld", "not approved"), f"plugin list: {listed}")
            row = driver.ui_wait('[data-plugin="uix"]', "Withheld")
            check("Not approved" in row, f"the row does not say not approved: {row}")

            # Approve the update from the row.
            driver.ui_click('[data-plugin="uix"] button', "Approve")
            trust = driver.ui_wait(".trust-view", "echo uix-2")
            driver.ui_click(".trust-view button.primary", "Approve")
            answer_pages(["Approve"], Pending)
            driver.ui_wait(".notice-box", "Approved", limit=60)
            for _ in range(20):
                if plugin_list().get("uix") == ("on", "counted"):
                    break
                time.sleep(0.5)
            check(plugin_list().get("uix") == ("on", "counted"), f"plugin list: {plugin_list()}")

            # Uninstall, then undo.
            driver.ui_click("button[data-uninstall=uix]")
            driver.ui_click('[role=alertdialog] button.danger', "Uninstall")
            driver.ui_wait(".notice-box", "Uninstalled uix")
            for _ in range(20):
                if "uix" not in plugin_list():
                    break
                time.sleep(0.5)
            check("uix" not in plugin_list(), f"still installed: {plugin_list()}")
            driver.ui_click(".notice-box button", "Undo")
            for _ in range(20):
                if plugin_list().get("uix") == ("on", "counted"):
                    break
                time.sleep(0.5)
            check(plugin_list().get("uix") == ("on", "counted"), f"undo did not restore it: {plugin_list()}")
            agent.stop()
            agent = None

        scenario("approve, polkit passes: counted", approve_counted)
        scenario("cancel on page 2 of a multi-page plugin: nothing installed", cancel_on_page_two)
        scenario("polkit refuses (wrong password): nothing recorded", refused)
        scenario("no authentication agent: dialog-only", no_agent)
        scenario("action file removed: dialog-only", no_action_file)
        scenario("Plugins window: install, approve, enable, update, uninstall, undo", plugins_window)
        driver.close()
        driver = None

        def refused_variable():
            preloaded = Driver("ld-preload", {"LD_PRELOAD": "x"})
            try:
                wait = preloaded.start_approve(f"zeta@{MARKET_NAME}", "install")
                no_dialog_appears()
                answer = wait()
                check(not answer["approved"], f"{answer}")
                check("LD_PRELOAD" in (answer["reason"] or ""), f"{answer}")
                check(not answer["installed"], f"{answer}")
            finally:
                preloaded.close()
            check("zeta" not in plugin_list(), "a refused app installed a plugin")

        scenario("LD_PRELOAD in the app's environment: refused, nothing sent", refused_variable)
        log("all presence end-to-end scenarios passed")
    finally:
        if agent:
            agent.stop()
        if driver:
            driver.close()
        if shutil.which("import"):
            subprocess.run(["import", "-window", "root", str(LOGS / "screen.png")], check=False)
        daemon.terminate()
        try:
            daemon.wait(timeout=30)
        except subprocess.TimeoutExpired:
            daemon.kill()


if __name__ == "__main__":
    try:
        main()
    except Failed as failure:
        log(f"FAILED: {failure}")
        sys.exit(1)
