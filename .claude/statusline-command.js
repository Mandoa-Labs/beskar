#!/usr/bin/env node

// Claude Code Statusline — inspired by GSD (get-shit-done)

// Shows: model | task | git | context bar | usage (5h/7d) | cost | duration

const fs = require('fs');
const path = require('path');
const os = require('os');
const https = require('https');
const { execSync } = require('child_process');

let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => input += chunk);
process.stdin.on('end', () => {
  try {
    const data = JSON.parse(input);
    const model = data.model?.display_name || 'Claude';
    const dir = data.workspace?.current_dir || process.cwd();
    const session = data.session_id || '';
    const remaining = data.context_window?.remaining_percentage;
    const cost = data.cost?.total_cost_usd || 0;
    const durationMs = data.cost?.total_duration_ms || 0;
    const linesAdded = data.cost?.total_lines_added || 0;
    const linesRemoved = data.cost?.total_lines_removed || 0;

    // ANSI helpers — bright/bold variants for purple background
    const DIM = '\x1b[1;37m'; // bold white instead of dim (dim is invisible on purple)
    const BOLD = '\x1b[1;97m'; // bold bright white
    const GREEN = '\x1b[1;92m'; // bright green
    const YELLOW = '\x1b[1;93m'; // bright yellow
    const RED = '\x1b[1;91m'; // bright red
    const CYAN = '\x1b[1;96m'; // bright cyan
    const ORANGE = '\x1b[1;38;5;208m'; // bold orange
    const BLINK_RED = '\x1b[5;91m'; // blink bright red
    const RESET = '\x1b[0m';
    const SEP = ` ${BOLD}\u2502${RESET} `;

    // --- Plan usage (cached, async-safe via file) ---
    let usageSection = '';
    const usageCacheFile = path.join(os.tmpdir(), 'claude-statusline-usage-cache.json');
    const USAGE_CACHE_TTL = 60000; // 60 seconds

    let usageCached = null;
    try {
      if (fs.existsSync(usageCacheFile)) {
        usageCached = JSON.parse(fs.readFileSync(usageCacheFile, 'utf8'));
        if (Date.now() - usageCached.ts > USAGE_CACHE_TTL) usageCached = null;
      }
    } catch (e) { usageCached = null; }

    // Fire-and-forget refresh when cache is stale
    if (!usageCached) {
      try {
        const credsPath = path.join(os.homedir(), '.claude', '.credentials.json');
        if (fs.existsSync(credsPath)) {
          const creds = JSON.parse(fs.readFileSync(credsPath, 'utf8'));
          const token = creds.claudeAiOauth?.accessToken;
          if (token) {
            const req = https.request('https://api.anthropic.com/api/oauth/usage', {
              method: 'GET',
              headers: {
                'Authorization': `Bearer ${token}`,
                'User-Agent': 'claude-code/2.0.31',
                'anthropic-beta': 'oauth-2025-04-20',
              },
              timeout: 3000,
            }, (res) => {
              let body = '';
              res.on('data', (d) => body += d);
              res.on('end', () => {
                try {
                  const usage = JSON.parse(body);
                  const cache = {
                    ts: Date.now(),
                    fiveHour: usage.five_hour?.utilization ?? null,
                    fiveHourResets: usage.five_hour?.resets_at ?? null,
                    sevenDay: usage.seven_day?.utilization ?? null,
                    sevenDayResets: usage.seven_day?.resets_at ?? null,
                  };
                  fs.writeFileSync(usageCacheFile, JSON.stringify(cache));
                } catch (e) { /* silent */ }
              });
            });
            req.on('error', () => {});
            req.on('timeout', () => req.destroy());
            req.end();
          }
        }
      } catch (e) { /* silent */ }
    }

    // Render usage from cache (will show on next render if just refreshed)
    if (usageCached) {
      const colorForPct = (pct) => {
        if (pct < 50) return GREEN;
        if (pct < 75) return YELLOW;
        if (pct < 90) return ORANGE;
        return BLINK_RED;
      };
      const parts = [];
      if (usageCached.fiveHour != null) {
        const pct = Math.round(usageCached.fiveHour);
        let resetStr = '';
        if (usageCached.fiveHourResets) {
          const resetMs = new Date(usageCached.fiveHourResets).getTime() - Date.now();
          if (resetMs > 0) {
            const h = Math.floor(resetMs / 3600000);
            const m = Math.floor((resetMs % 3600000) / 60000);
            resetStr = ` ${DIM}${h}h${m.toString().padStart(2, '0')}m${RESET}`;
          }
        }
        parts.push(`${colorForPct(pct)}5h:${pct}%${RESET}${resetStr}`);
      }
      if (usageCached.sevenDay != null) {
        const pct = Math.round(usageCached.sevenDay);
        parts.push(`${colorForPct(pct)}7d:${pct}%${RESET}`);
      }
      if (parts.length > 0) usageSection = parts.join(' ');
    }

    // --- Context bar (GSD-style: scale to 80% real limit) ---
    let ctxSection = '';
    if (remaining != null) {
      const rawUsed = Math.max(0, Math.min(100, 100 - Math.round(remaining)));
      const used = Math.min(100, Math.round((rawUsed / 80) * 100));
      const filled = Math.floor(used / 10);
      const bar = '\u2588'.repeat(filled) + '\u2591'.repeat(10 - filled);
      let color;
      if (used < 63) color = GREEN;
      else if (used < 81) color = YELLOW;
      else if (used < 95) color = ORANGE;
      else color = BLINK_RED;
      ctxSection = `${color}${bar} ${used}%${RESET}`;
    }

    // --- Current task from todos (GSD pattern) ---
    let taskSection = '';
    const todosDir = path.join(os.homedir(), '.claude', 'todos');
    if (session && fs.existsSync(todosDir)) {
      try {
        const files = fs.readdirSync(todosDir)
          .filter(f => f.startsWith(session) && f.endsWith('.json'))
          .map(f => ({ name: f, mtime: fs.statSync(path.join(todosDir, f)).mtime }))
          .sort((a, b) => b.mtime - a.mtime);

        if (files.length > 0) {
          const todos = JSON.parse(fs.readFileSync(path.join(todosDir, files[0].name), 'utf8'));
          const inProgress = todos.find(t => t.status === 'in_progress');
          if (inProgress) {
            taskSection = `${BOLD}${inProgress.activeForm || inProgress.subject || ''}${RESET}`;
          }
        }
      } catch (e) { /* silent */ }
    }

    // --- Git info (cached to avoid slowness) ---
    let gitSection = '';
    const cacheFile = path.join(os.tmpdir(), 'claude-statusline-git-cache.json');
    const CACHE_TTL = 5000; // 5 seconds

    let cached = null;
    try {
      if (fs.existsSync(cacheFile)) {
        cached = JSON.parse(fs.readFileSync(cacheFile, 'utf8'));
        if (Date.now() - cached.ts > CACHE_TTL || cached.dir !== dir) cached = null;
      }
    } catch (e) { cached = null; }

    if (!cached) {
      try {
        execSync('git rev-parse --git-dir', { cwd: dir, stdio: 'ignore' });
        const branch = execSync('git branch --show-current', { cwd: dir, encoding: 'utf8', stdio: ['pipe', 'pipe', 'ignore'] }).trim();
        const staged = execSync('git diff --cached --name-only', { cwd: dir, encoding: 'utf8', stdio: ['pipe', 'pipe', 'ignore'] }).trim();
        const modified = execSync('git diff --name-only', { cwd: dir, encoding: 'utf8', stdio: ['pipe', 'pipe', 'ignore'] }).trim();
        const stagedCount = staged ? staged.split('\n').length : 0;
        const modifiedCount = modified ? modified.split('\n').length : 0;
        cached = { ts: Date.now(), dir, branch, stagedCount, modifiedCount };
        fs.writeFileSync(cacheFile, JSON.stringify(cached));
      } catch (e) {
        cached = { ts: Date.now(), dir, branch: '', stagedCount: 0, modifiedCount: 0 };
        fs.writeFileSync(cacheFile, JSON.stringify(cached));
      }
    }

    if (cached.branch) {
      let indicators = '';
      if (cached.stagedCount > 0) indicators += ` ${GREEN}+${cached.stagedCount}${RESET}`;
      if (cached.modifiedCount > 0) indicators += ` ${YELLOW}~${cached.modifiedCount}${RESET}`;
      gitSection = `${CYAN}${cached.branch}${RESET}${indicators}`;
    }

    // --- Cost ---
    const costStr = cost > 0 ? `${cost.toFixed(2)}` : '$0.00';

    // --- Duration ---
    const totalSec = Math.floor(durationMs / 1000);
    const mins = Math.floor(totalSec / 60);
    const secs = totalSec % 60;
    const durationStr = `${mins}m${secs.toString().padStart(2, '0')}s`;

    // --- Lines changed ---
    let linesStr = '';
    if (linesAdded > 0 || linesRemoved > 0) {
      linesStr = `${GREEN}+${linesAdded}${RESET}/${RED}-${linesRemoved}${RESET}`;
    }

    // --- Assemble line 1: model | task | git | lines | dir ---
    const parts1 = [`${DIM}${model}${RESET}`];
    if (taskSection) parts1.push(taskSection);
    if (gitSection) parts1.push(gitSection);
    if (linesStr) parts1.push(linesStr);
    parts1.push(`${DIM}${path.basename(dir)}${RESET}`);

    // --- Assemble line 2: context bar | usage | cost | duration ---
    const parts2 = [];
    if (ctxSection) parts2.push(ctxSection);
    if (usageSection) parts2.push(usageSection);
    parts2.push(`${DIM}${costStr}${RESET}`);
    parts2.push(`${DIM}${durationStr}${RESET}`);

    process.stdout.write(parts1.join(SEP) + '\n' + parts2.join(SEP));
  } catch (e) {
    // Silent fail — never break the statusline
  }
});
