// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

// 1. Configuration
const CONFIG = {
  msalClientId: '',
  msalAuthority: '',
  storageAccount: '',
  functionAppName: '',
  actualStateTable: 'actualstate',
  desiredStateTable: 'desiredstate',
  programsTable: 'programs',
  sensorDataTable: 'sensordata',
  gatewayEscrowTable: 'gatewayescrow',
  refreshIntervalMs: 30000,
};

const ENV_STORAGE_KEY = 'sonde_environments';
const ENV_ACTIVE_KEY = 'sonde_active_environment';

function loadEnvironments() {
  try {
    const raw = localStorage.getItem(ENV_STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function saveEnvironments(envs) {
  try {
    localStorage.setItem(ENV_STORAGE_KEY, JSON.stringify(envs));
    return true;
  } catch {
    return false;
  }
}

function getActiveEnvironmentName() {
  try {
    return localStorage.getItem(ENV_ACTIVE_KEY) || '';
  } catch {
    return '';
  }
}

function setActiveEnvironmentName(name) {
  try {
    localStorage.setItem(ENV_ACTIVE_KEY, name);
  } catch {
    // Storage disabled or quota exceeded.
  }
}

function applyEnvironment(env) {
  if (!env) return;
  CONFIG.msalClientId = env.clientId || '';
  CONFIG.msalAuthority = env.tenantId
    ? `https://login.microsoftonline.com/${env.tenantId}`
    : '';
  CONFIG.storageAccount = env.storageAccount || '';
  CONFIG.functionAppName = env.functionAppName || '';
}

function loadActiveEnvironment() {
  const envs = loadEnvironments();
  const activeName = getActiveEnvironmentName();
  const env = envs.find((e) => e.name === activeName) || envs[0] || null;
  if (env) {
    setActiveEnvironmentName(env.name);
    applyEnvironment(env);
  }
  return env;
}

const STORAGE_SCOPES = ['https://storage.azure.com/.default'];
function functionScopes() {
  return [`api://${CONFIG.msalClientId}/user_impersonation`];
}
const TAB_IDS = ['dashboard', 'desired-state', 'programs', 'sensor-data', 'gateway'];
const APP = {
  msalApp: null,
  account: null,
  activeTab: 'dashboard',
  refreshHandle: null,
  refreshToken: 0,
  viewMessage: null,
  sensorChart: null,
};

// BIP-39 English wordlist (2048 entries) for key fingerprint computation.
const BIP39_ENGLISH = [
  "abandon", "ability", "able", "about", "above", "absent", "absorb", "abstract", "absurd", "abuse", "access", "accident", "account", "accuse", "achieve", "acid"
  , "acoustic", "acquire", "across", "act", "action", "actor", "actress", "actual", "adapt", "add", "addict", "address", "adjust", "admit", "adult", "advance"
  , "advice", "aerobic", "affair", "afford", "afraid", "again", "age", "agent", "agree", "ahead", "aim", "air", "airport", "aisle", "alarm", "album"
  , "alcohol", "alert", "alien", "all", "alley", "allow", "almost", "alone", "alpha", "already", "also", "alter", "always", "amateur", "amazing", "among"
  , "amount", "amused", "analyst", "anchor", "ancient", "anger", "angle", "angry", "animal", "ankle", "announce", "annual", "another", "answer", "antenna", "antique"
  , "anxiety", "any", "apart", "apology", "appear", "apple", "approve", "april", "arch", "arctic", "area", "arena", "argue", "arm", "armed", "armor"
  , "army", "around", "arrange", "arrest", "arrive", "arrow", "art", "artefact", "artist", "artwork", "ask", "aspect", "assault", "asset", "assist", "assume"
  , "asthma", "athlete", "atom", "attack", "attend", "attitude", "attract", "auction", "audit", "august", "aunt", "author", "auto", "autumn", "average", "avocado"
  , "avoid", "awake", "aware", "away", "awesome", "awful", "awkward", "axis", "baby", "bachelor", "bacon", "badge", "bag", "balance", "balcony", "ball"
  , "bamboo", "banana", "banner", "bar", "barely", "bargain", "barrel", "base", "basic", "basket", "battle", "beach", "bean", "beauty", "because", "become"
  , "beef", "before", "begin", "behave", "behind", "believe", "below", "belt", "bench", "benefit", "best", "betray", "better", "between", "beyond", "bicycle"
  , "bid", "bike", "bind", "biology", "bird", "birth", "bitter", "black", "blade", "blame", "blanket", "blast", "bleak", "bless", "blind", "blood"
  , "blossom", "blouse", "blue", "blur", "blush", "board", "boat", "body", "boil", "bomb", "bone", "bonus", "book", "boost", "border", "boring"
  , "borrow", "boss", "bottom", "bounce", "box", "boy", "bracket", "brain", "brand", "brass", "brave", "bread", "breeze", "brick", "bridge", "brief"
  , "bright", "bring", "brisk", "broccoli", "broken", "bronze", "broom", "brother", "brown", "brush", "bubble", "buddy", "budget", "buffalo", "build", "bulb"
  , "bulk", "bullet", "bundle", "bunker", "burden", "burger", "burst", "bus", "business", "busy", "butter", "buyer", "buzz", "cabbage", "cabin", "cable"
  , "cactus", "cage", "cake", "call", "calm", "camera", "camp", "can", "canal", "cancel", "candy", "cannon", "canoe", "canvas", "canyon", "capable"
  , "capital", "captain", "car", "carbon", "card", "cargo", "carpet", "carry", "cart", "case", "cash", "casino", "castle", "casual", "cat", "catalog"
  , "catch", "category", "cattle", "caught", "cause", "caution", "cave", "ceiling", "celery", "cement", "census", "century", "cereal", "certain", "chair", "chalk"
  , "champion", "change", "chaos", "chapter", "charge", "chase", "chat", "cheap", "check", "cheese", "chef", "cherry", "chest", "chicken", "chief", "child"
  , "chimney", "choice", "choose", "chronic", "chuckle", "chunk", "churn", "cigar", "cinnamon", "circle", "citizen", "city", "civil", "claim", "clap", "clarify"
  , "claw", "clay", "clean", "clerk", "clever", "click", "client", "cliff", "climb", "clinic", "clip", "clock", "clog", "close", "cloth", "cloud"
  , "clown", "club", "clump", "cluster", "clutch", "coach", "coast", "coconut", "code", "coffee", "coil", "coin", "collect", "color", "column", "combine"
  , "come", "comfort", "comic", "common", "company", "concert", "conduct", "confirm", "congress", "connect", "consider", "control", "convince", "cook", "cool", "copper"
  , "copy", "coral", "core", "corn", "correct", "cost", "cotton", "couch", "country", "couple", "course", "cousin", "cover", "coyote", "crack", "cradle"
  , "craft", "cram", "crane", "crash", "crater", "crawl", "crazy", "cream", "credit", "creek", "crew", "cricket", "crime", "crisp", "critic", "crop"
  , "cross", "crouch", "crowd", "crucial", "cruel", "cruise", "crumble", "crunch", "crush", "cry", "crystal", "cube", "culture", "cup", "cupboard", "curious"
  , "current", "curtain", "curve", "cushion", "custom", "cute", "cycle", "dad", "damage", "damp", "dance", "danger", "daring", "dash", "daughter", "dawn"
  , "day", "deal", "debate", "debris", "decade", "december", "decide", "decline", "decorate", "decrease", "deer", "defense", "define", "defy", "degree", "delay"
  , "deliver", "demand", "demise", "denial", "dentist", "deny", "depart", "depend", "deposit", "depth", "deputy", "derive", "describe", "desert", "design", "desk"
  , "despair", "destroy", "detail", "detect", "develop", "device", "devote", "diagram", "dial", "diamond", "diary", "dice", "diesel", "diet", "differ", "digital"
  , "dignity", "dilemma", "dinner", "dinosaur", "direct", "dirt", "disagree", "discover", "disease", "dish", "dismiss", "disorder", "display", "distance", "divert", "divide"
  , "divorce", "dizzy", "doctor", "document", "dog", "doll", "dolphin", "domain", "donate", "donkey", "donor", "door", "dose", "double", "dove", "draft"
  , "dragon", "drama", "drastic", "draw", "dream", "dress", "drift", "drill", "drink", "drip", "drive", "drop", "drum", "dry", "duck", "dumb"
  , "dune", "during", "dust", "dutch", "duty", "dwarf", "dynamic", "eager", "eagle", "early", "earn", "earth", "easily", "east", "easy", "echo"
  , "ecology", "economy", "edge", "edit", "educate", "effort", "egg", "eight", "either", "elbow", "elder", "electric", "elegant", "element", "elephant", "elevator"
  , "elite", "else", "embark", "embody", "embrace", "emerge", "emotion", "employ", "empower", "empty", "enable", "enact", "end", "endless", "endorse", "enemy"
  , "energy", "enforce", "engage", "engine", "enhance", "enjoy", "enlist", "enough", "enrich", "enroll", "ensure", "enter", "entire", "entry", "envelope", "episode"
  , "equal", "equip", "era", "erase", "erode", "erosion", "error", "erupt", "escape", "essay", "essence", "estate", "eternal", "ethics", "evidence", "evil"
  , "evoke", "evolve", "exact", "example", "excess", "exchange", "excite", "exclude", "excuse", "execute", "exercise", "exhaust", "exhibit", "exile", "exist", "exit"
  , "exotic", "expand", "expect", "expire", "explain", "expose", "express", "extend", "extra", "eye", "eyebrow", "fabric", "face", "faculty", "fade", "faint"
  , "faith", "fall", "false", "fame", "family", "famous", "fan", "fancy", "fantasy", "farm", "fashion", "fat", "fatal", "father", "fatigue", "fault"
  , "favorite", "feature", "february", "federal", "fee", "feed", "feel", "female", "fence", "festival", "fetch", "fever", "few", "fiber", "fiction", "field"
  , "figure", "file", "film", "filter", "final", "find", "fine", "finger", "finish", "fire", "firm", "first", "fiscal", "fish", "fit", "fitness"
  , "fix", "flag", "flame", "flash", "flat", "flavor", "flee", "flight", "flip", "float", "flock", "floor", "flower", "fluid", "flush", "fly"
  , "foam", "focus", "fog", "foil", "fold", "follow", "food", "foot", "force", "forest", "forget", "fork", "fortune", "forum", "forward", "fossil"
  , "foster", "found", "fox", "fragile", "frame", "frequent", "fresh", "friend", "fringe", "frog", "front", "frost", "frown", "frozen", "fruit", "fuel"
  , "fun", "funny", "furnace", "fury", "future", "gadget", "gain", "galaxy", "gallery", "game", "gap", "garage", "garbage", "garden", "garlic", "garment"
  , "gas", "gasp", "gate", "gather", "gauge", "gaze", "general", "genius", "genre", "gentle", "genuine", "gesture", "ghost", "giant", "gift", "giggle"
  , "ginger", "giraffe", "girl", "give", "glad", "glance", "glare", "glass", "glide", "glimpse", "globe", "gloom", "glory", "glove", "glow", "glue"
  , "goat", "goddess", "gold", "good", "goose", "gorilla", "gospel", "gossip", "govern", "gown", "grab", "grace", "grain", "grant", "grape", "grass"
  , "gravity", "great", "green", "grid", "grief", "grit", "grocery", "group", "grow", "grunt", "guard", "guess", "guide", "guilt", "guitar", "gun"
  , "gym", "habit", "hair", "half", "hammer", "hamster", "hand", "happy", "harbor", "hard", "harsh", "harvest", "hat", "have", "hawk", "hazard"
  , "head", "health", "heart", "heavy", "hedgehog", "height", "hello", "helmet", "help", "hen", "hero", "hidden", "high", "hill", "hint", "hip"
  , "hire", "history", "hobby", "hockey", "hold", "hole", "holiday", "hollow", "home", "honey", "hood", "hope", "horn", "horror", "horse", "hospital"
  , "host", "hotel", "hour", "hover", "hub", "huge", "human", "humble", "humor", "hundred", "hungry", "hunt", "hurdle", "hurry", "hurt", "husband"
  , "hybrid", "ice", "icon", "idea", "identify", "idle", "ignore", "ill", "illegal", "illness", "image", "imitate", "immense", "immune", "impact", "impose"
  , "improve", "impulse", "inch", "include", "income", "increase", "index", "indicate", "indoor", "industry", "infant", "inflict", "inform", "inhale", "inherit", "initial"
  , "inject", "injury", "inmate", "inner", "innocent", "input", "inquiry", "insane", "insect", "inside", "inspire", "install", "intact", "interest", "into", "invest"
  , "invite", "involve", "iron", "island", "isolate", "issue", "item", "ivory", "jacket", "jaguar", "jar", "jazz", "jealous", "jeans", "jelly", "jewel"
  , "job", "join", "joke", "journey", "joy", "judge", "juice", "jump", "jungle", "junior", "junk", "just", "kangaroo", "keen", "keep", "ketchup"
  , "key", "kick", "kid", "kidney", "kind", "kingdom", "kiss", "kit", "kitchen", "kite", "kitten", "kiwi", "knee", "knife", "knock", "know"
  , "lab", "label", "labor", "ladder", "lady", "lake", "lamp", "language", "laptop", "large", "later", "latin", "laugh", "laundry", "lava", "law"
  , "lawn", "lawsuit", "layer", "lazy", "leader", "leaf", "learn", "leave", "lecture", "left", "leg", "legal", "legend", "leisure", "lemon", "lend"
  , "length", "lens", "leopard", "lesson", "letter", "level", "liar", "liberty", "library", "license", "life", "lift", "light", "like", "limb", "limit"
  , "link", "lion", "liquid", "list", "little", "live", "lizard", "load", "loan", "lobster", "local", "lock", "logic", "lonely", "long", "loop"
  , "lottery", "loud", "lounge", "love", "loyal", "lucky", "luggage", "lumber", "lunar", "lunch", "luxury", "lyrics", "machine", "mad", "magic", "magnet"
  , "maid", "mail", "main", "major", "make", "mammal", "man", "manage", "mandate", "mango", "mansion", "manual", "maple", "marble", "march", "margin"
  , "marine", "market", "marriage", "mask", "mass", "master", "match", "material", "math", "matrix", "matter", "maximum", "maze", "meadow", "mean", "measure"
  , "meat", "mechanic", "medal", "media", "melody", "melt", "member", "memory", "mention", "menu", "mercy", "merge", "merit", "merry", "mesh", "message"
  , "metal", "method", "middle", "midnight", "milk", "million", "mimic", "mind", "minimum", "minor", "minute", "miracle", "mirror", "misery", "miss", "mistake"
  , "mix", "mixed", "mixture", "mobile", "model", "modify", "mom", "moment", "monitor", "monkey", "monster", "month", "moon", "moral", "more", "morning"
  , "mosquito", "mother", "motion", "motor", "mountain", "mouse", "move", "movie", "much", "muffin", "mule", "multiply", "muscle", "museum", "mushroom", "music"
  , "must", "mutual", "myself", "mystery", "myth", "naive", "name", "napkin", "narrow", "nasty", "nation", "nature", "near", "neck", "need", "negative"
  , "neglect", "neither", "nephew", "nerve", "nest", "net", "network", "neutral", "never", "news", "next", "nice", "night", "noble", "noise", "nominee"
  , "noodle", "normal", "north", "nose", "notable", "note", "nothing", "notice", "novel", "now", "nuclear", "number", "nurse", "nut", "oak", "obey"
  , "object", "oblige", "obscure", "observe", "obtain", "obvious", "occur", "ocean", "october", "odor", "off", "offer", "office", "often", "oil", "okay"
  , "old", "olive", "olympic", "omit", "once", "one", "onion", "online", "only", "open", "opera", "opinion", "oppose", "option", "orange", "orbit"
  , "orchard", "order", "ordinary", "organ", "orient", "original", "orphan", "ostrich", "other", "outdoor", "outer", "output", "outside", "oval", "oven", "over"
  , "own", "owner", "oxygen", "oyster", "ozone", "pact", "paddle", "page", "pair", "palace", "palm", "panda", "panel", "panic", "panther", "paper"
  , "parade", "parent", "park", "parrot", "party", "pass", "patch", "path", "patient", "patrol", "pattern", "pause", "pave", "payment", "peace", "peanut"
  , "pear", "peasant", "pelican", "pen", "penalty", "pencil", "people", "pepper", "perfect", "permit", "person", "pet", "phone", "photo", "phrase", "physical"
  , "piano", "picnic", "picture", "piece", "pig", "pigeon", "pill", "pilot", "pink", "pioneer", "pipe", "pistol", "pitch", "pizza", "place", "planet"
  , "plastic", "plate", "play", "please", "pledge", "pluck", "plug", "plunge", "poem", "poet", "point", "polar", "pole", "police", "pond", "pony"
  , "pool", "popular", "portion", "position", "possible", "post", "potato", "pottery", "poverty", "powder", "power", "practice", "praise", "predict", "prefer", "prepare"
  , "present", "pretty", "prevent", "price", "pride", "primary", "print", "priority", "prison", "private", "prize", "problem", "process", "produce", "profit", "program"
  , "project", "promote", "proof", "property", "prosper", "protect", "proud", "provide", "public", "pudding", "pull", "pulp", "pulse", "pumpkin", "punch", "pupil"
  , "puppy", "purchase", "purity", "purpose", "purse", "push", "put", "puzzle", "pyramid", "quality", "quantum", "quarter", "question", "quick", "quit", "quiz"
  , "quote", "rabbit", "raccoon", "race", "rack", "radar", "radio", "rail", "rain", "raise", "rally", "ramp", "ranch", "random", "range", "rapid"
  , "rare", "rate", "rather", "raven", "raw", "razor", "ready", "real", "reason", "rebel", "rebuild", "recall", "receive", "recipe", "record", "recycle"
  , "reduce", "reflect", "reform", "refuse", "region", "regret", "regular", "reject", "relax", "release", "relief", "rely", "remain", "remember", "remind", "remove"
  , "render", "renew", "rent", "reopen", "repair", "repeat", "replace", "report", "require", "rescue", "resemble", "resist", "resource", "response", "result", "retire"
  , "retreat", "return", "reunion", "reveal", "review", "reward", "rhythm", "rib", "ribbon", "rice", "rich", "ride", "ridge", "rifle", "right", "rigid"
  , "ring", "riot", "ripple", "risk", "ritual", "rival", "river", "road", "roast", "robot", "robust", "rocket", "romance", "roof", "rookie", "room"
  , "rose", "rotate", "rough", "round", "route", "royal", "rubber", "rude", "rug", "rule", "run", "runway", "rural", "sad", "saddle", "sadness"
  , "safe", "sail", "salad", "salmon", "salon", "salt", "salute", "same", "sample", "sand", "satisfy", "satoshi", "sauce", "sausage", "save", "say"
  , "scale", "scan", "scare", "scatter", "scene", "scheme", "school", "science", "scissors", "scorpion", "scout", "scrap", "screen", "script", "scrub", "sea"
  , "search", "season", "seat", "second", "secret", "section", "security", "seed", "seek", "segment", "select", "sell", "seminar", "senior", "sense", "sentence"
  , "series", "service", "session", "settle", "setup", "seven", "shadow", "shaft", "shallow", "share", "shed", "shell", "sheriff", "shield", "shift", "shine"
  , "ship", "shiver", "shock", "shoe", "shoot", "shop", "short", "shoulder", "shove", "shrimp", "shrug", "shuffle", "shy", "sibling", "sick", "side"
  , "siege", "sight", "sign", "silent", "silk", "silly", "silver", "similar", "simple", "since", "sing", "siren", "sister", "situate", "six", "size"
  , "skate", "sketch", "ski", "skill", "skin", "skirt", "skull", "slab", "slam", "sleep", "slender", "slice", "slide", "slight", "slim", "slogan"
  , "slot", "slow", "slush", "small", "smart", "smile", "smoke", "smooth", "snack", "snake", "snap", "sniff", "snow", "soap", "soccer", "social"
  , "sock", "soda", "soft", "solar", "soldier", "solid", "solution", "solve", "someone", "song", "soon", "sorry", "sort", "soul", "sound", "soup"
  , "source", "south", "space", "spare", "spatial", "spawn", "speak", "special", "speed", "spell", "spend", "sphere", "spice", "spider", "spike", "spin"
  , "spirit", "split", "spoil", "sponsor", "spoon", "sport", "spot", "spray", "spread", "spring", "spy", "square", "squeeze", "squirrel", "stable", "stadium"
  , "staff", "stage", "stairs", "stamp", "stand", "start", "state", "stay", "steak", "steel", "stem", "step", "stereo", "stick", "still", "sting"
  , "stock", "stomach", "stone", "stool", "story", "stove", "strategy", "street", "strike", "strong", "struggle", "student", "stuff", "stumble", "style", "subject"
  , "submit", "subway", "success", "such", "sudden", "suffer", "sugar", "suggest", "suit", "summer", "sun", "sunny", "sunset", "super", "supply", "supreme"
  , "sure", "surface", "surge", "surprise", "surround", "survey", "suspect", "sustain", "swallow", "swamp", "swap", "swarm", "swear", "sweet", "swift", "swim"
  , "swing", "switch", "sword", "symbol", "symptom", "syrup", "system", "table", "tackle", "tag", "tail", "talent", "talk", "tank", "tape", "target"
  , "task", "taste", "tattoo", "taxi", "teach", "team", "tell", "ten", "tenant", "tennis", "tent", "term", "test", "text", "thank", "that"
  , "theme", "then", "theory", "there", "they", "thing", "this", "thought", "three", "thrive", "throw", "thumb", "thunder", "ticket", "tide", "tiger"
  , "tilt", "timber", "time", "tiny", "tip", "tired", "tissue", "title", "toast", "tobacco", "today", "toddler", "toe", "together", "toilet", "token"
  , "tomato", "tomorrow", "tone", "tongue", "tonight", "tool", "tooth", "top", "topic", "topple", "torch", "tornado", "tortoise", "toss", "total", "tourist"
  , "toward", "tower", "town", "toy", "track", "trade", "traffic", "tragic", "train", "transfer", "trap", "trash", "travel", "tray", "treat", "tree"
  , "trend", "trial", "tribe", "trick", "trigger", "trim", "trip", "trophy", "trouble", "truck", "true", "truly", "trumpet", "trust", "truth", "try"
  , "tube", "tuition", "tumble", "tuna", "tunnel", "turkey", "turn", "turtle", "twelve", "twenty", "twice", "twin", "twist", "two", "type", "typical"
  , "ugly", "umbrella", "unable", "unaware", "uncle", "uncover", "under", "undo", "unfair", "unfold", "unhappy", "uniform", "unique", "unit", "universe", "unknown"
  , "unlock", "until", "unusual", "unveil", "update", "upgrade", "uphold", "upon", "upper", "upset", "urban", "urge", "usage", "use", "used", "useful"
  , "useless", "usual", "utility", "vacant", "vacuum", "vague", "valid", "valley", "valve", "van", "vanish", "vapor", "various", "vast", "vault", "vehicle"
  , "velvet", "vendor", "venture", "venue", "verb", "verify", "version", "very", "vessel", "veteran", "viable", "vibrant", "vicious", "victory", "video", "view"
  , "village", "vintage", "violin", "virtual", "virus", "visa", "visit", "visual", "vital", "vivid", "vocal", "voice", "void", "volcano", "volume", "vote"
  , "voyage", "wage", "wagon", "wait", "walk", "wall", "walnut", "want", "warfare", "warm", "warrior", "wash", "wasp", "waste", "water", "wave"
  , "way", "wealth", "weapon", "wear", "weasel", "weather", "web", "wedding", "weekend", "weird", "welcome", "west", "wet", "whale", "what", "wheat"
  , "wheel", "when", "where", "whip", "whisper", "wide", "width", "wife", "wild", "will", "win", "window", "wine", "wing", "wink", "winner"
  , "winter", "wire", "wisdom", "wise", "wish", "witness", "wolf", "woman", "wonder", "wood", "wool", "word", "work", "world", "worry", "worth"
  , "wrap", "wreck", "wrestle", "wrist", "write", "wrong", "yard", "year", "yellow", "you", "young", "youth", "zebra", "zero", "zone", "zoo"
];

const contentEl = document.getElementById('content');
const authControlsEl = document.getElementById('auth-controls');

// 8. Utility Functions
async function sha256hex(text) {
  const data = new TextEncoder().encode(text);
  const digest = await crypto.subtle.digest('SHA-256', data);
  return [...new Uint8Array(digest)].map((value) => value.toString(16).padStart(2, '0')).join('');
}

function truncHash(hex) {
  return hex ? String(hex).slice(0, 8) : '—';
}

function relativeTime(timestampMs) {
  const value = Number(timestampMs);
  if (!Number.isFinite(value) || value <= 0) {
    return '—';
  }

  const diffMs = Date.now() - value;
  const diffSeconds = Math.max(0, Math.floor(diffMs / 1000));
  const steps = [
    ['d', 86400],
    ['h', 3600],
    ['m', 60],
  ];

  for (const [suffix, size] of steps) {
    if (diffSeconds >= size) {
      return `${Math.floor(diffSeconds / size)}${suffix} ago`;
    }
  }
  return `${diffSeconds}s ago`;
}

function randomHex(bytes) {
  const data = new Uint8Array(bytes);
  crypto.getRandomValues(data);
  return [...data].map((value) => value.toString(16).padStart(2, '0')).join('');
}

function escapeHtml(value) {
  return String(value ?? '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

function showViewMessage(kind, text) {
  APP.viewMessage = { kind, text };
}

function consumeViewMessage() {
  const message = APP.viewMessage;
  APP.viewMessage = null;
  return message;
}

function messageHtml(message) {
  if (!message) {
    return '';
  }
  const cssClass = message.kind === 'success' ? 'success' : 'error';
  return `<div class="alert ${cssClass}">${escapeHtml(message.text)}</div>`;
}

function renderCard(title, innerHtml) {
  const message = consumeViewMessage();
  contentEl.innerHTML = `
    <section class="stack">
      <div class="card stack">
        <div>
          <h1>${escapeHtml(title)}</h1>
        </div>
        ${messageHtml(message)}
        ${innerHtml}
      </div>
    </section>
  `;
}

function renderError(title, error) {
  const text = error instanceof Error ? error.message : String(error);
  renderCard(title, `<div class="alert error">${escapeHtml(text)}</div>`);
}

function clearRefresh() {
  APP.refreshToken += 1;
  if (APP.refreshHandle != null) {
    clearTimeout(APP.refreshHandle);
    APP.refreshHandle = null;
  }
}

function setAutoRefresh(callback) {
  clearRefresh();
  const refreshToken = APP.refreshToken;

  async function tick() {
    try {
      await callback();
    } catch (error) {
      renderError('Refresh failed', error);
    } finally {
      if (APP.refreshToken === refreshToken) {
        APP.refreshHandle = setTimeout(tick, CONFIG.refreshIntervalMs);
      }
    }
  }

  APP.refreshHandle = setTimeout(tick, CONFIG.refreshIntervalMs);
}

function latestByPartition(entities) {
  const grouped = new Map();
  for (const entity of entities) {
    const existing = grouped.get(entity.PartitionKey);
    if (!existing || String(entity.RowKey) < String(existing.RowKey)) {
      grouped.set(entity.PartitionKey, entity);
    }
  }
  return [...grouped.values()];
}

function sortByDateDesc(entities, field) {
  return [...entities].sort((left, right) => String(right[field] ?? '').localeCompare(String(left[field] ?? '')));
}

function requireConfig(key, label) {
  if (!CONFIG[key]) {
    throw new Error(`${label} is not configured. Open the environment manager to set it.`);
  }
}

function formatHashCell(hash) {
  if (!hash) {
    return '—';
  }
  return `<code title="${escapeHtml(hash)}">${escapeHtml(truncHash(hash))}</code>`;
}

function parseErrorPayload(payload, fallback) {
  if (!payload) {
    return fallback;
  }
  if (payload instanceof Error) {
    return payload.message || fallback;
  }
  if (typeof payload === 'string') {
    return payload;
  }
  if (payload.error) {
    return typeof payload.error === 'string' ? payload.error : JSON.stringify(payload.error);
  }
  if (payload.message) {
    return payload.message;
  }
  return JSON.stringify(payload);
}

// Base64 encode/decode helpers for crypto operations
function bytesToBase64(bytes) {
  return btoa(String.fromCharCode(...bytes));
}

function base64ToBytes(b64) {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

function concatBytes(...arrays) {
  const total = arrays.reduce((sum, array) => sum + array.length, 0);
  const result = new Uint8Array(total);
  let offset = 0;
  for (const array of arrays) {
    result.set(array, offset);
    offset += array.length;
  }
  return result;
}

// Compute 6-word BIP-39 fingerprint from X25519 public key bytes
async function computeFingerprint(publicKeyBytes) {
  const hashBuffer = await crypto.subtle.digest('SHA-256', publicKeyBytes);
  const hash = new Uint8Array(hashBuffer);
  let bits = 0n;
  for (let i = 0; i < 9; i++) {
    bits = (bits << 8n) | BigInt(hash[i]);
  }
  const words = [];
  for (let i = 0; i < 6; i++) {
    const shift = 72n - 11n - 11n * BigInt(i);
    const index = Number((bits >> shift) & 0x7FFn);
    words.push(BIP39_ENGLISH[index]);
  }
  return words;
}

// HKDF-SHA-256 using Web Crypto
async function hkdfSha256(ikm, salt, info, length) {
  const saltBytes = typeof salt === 'string' ? new TextEncoder().encode(salt) : salt;
  const baseKey = await crypto.subtle.importKey('raw', ikm, 'HKDF', false, ['deriveBits']);
  const bits = await crypto.subtle.deriveBits(
    { name: 'HKDF', hash: 'SHA-256', salt: saltBytes, info },
    baseKey,
    length * 8,
  );
  return new Uint8Array(bits);
}

// Send admin command to handler
async function sendAdminCommand(command, params) {
  requireConfig('functionAppName', 'Function app name');
  const token = await getFunctionToken();
  const response = await fetch(
    `https://${CONFIG.functionAppName}.azurewebsites.net/api/admin/command`,
    {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${token}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({ command, params: params || {} }),
    },
  );
  const text = await response.text();
  let result = null;
  if (text) {
    try {
      result = JSON.parse(text);
    } catch {
      result = text;
    }
  }
  if (!response.ok) {
    throw new Error(parseErrorPayload(result, `Admin command failed (${response.status}).`));
  }
  return result;
}

// Delete program from handler
async function deleteProgramFromFunction(programHash) {
  requireConfig('functionAppName', 'Function app name');
  const token = await getFunctionToken();
  const response = await fetch(
    `https://${CONFIG.functionAppName}.azurewebsites.net/api/programs/${encodeURIComponent(programHash)}`,
    {
      method: 'DELETE',
      headers: { Authorization: `Bearer ${token}` },
    },
  );
  const text = await response.text();
  let result = null;
  if (text) {
    try {
      result = JSON.parse(text);
    } catch {
      result = text;
    }
  }
  if (!response.ok) {
    throw new Error(parseErrorPayload(result, `Delete failed (${response.status}).`));
  }
  return result;
}

// Read entity from Azure Table by partition key + row key
async function getEntity(tableName, partitionKey, rowKey) {
  const token = await getToken();
  const url = entityUrl(tableName, partitionKey, rowKey);
  const response = await fetch(url, {
    method: 'GET',
    headers: {
      Accept: 'application/json;odata=nometadata',
      Authorization: `Bearer ${token}`,
      'x-ms-version': '2019-02-02',
    },
  });
  if (response.status === 404) {
    return null;
  }
  if (!response.ok) {
    const text = await response.text();
    throw new Error(`Entity query failed (${response.status}): ${text}`);
  }
  return response.json();
}

// 2. MSAL Authentication
async function initMsal() {
  if (!window.msal || !CONFIG.msalClientId || !CONFIG.msalAuthority) {
    updateAuthUi();
    return;
  }

  // Normalize pathname to directory (strip filename like index.html) so the
  // redirect URI matches the registered value (e.g. /sonde/ not /sonde/index.html).
  const basePath = window.location.pathname.replace(/\/[^/]*\.[^/]*$/, '/');

  // The SPA uses hash-based routing (#dashboard, #sensor-data, etc.) but
  // MSAL reads window.location.hash during construction and handleRedirectPromise().
  // Temporarily clear the routing hash so MSAL doesn't try to parse it as an
  // auth response.  Auth hashes (containing code=, error=, etc.) are left in place.
  const currentHash = window.location.hash;
  const isAuthHash = currentHash && (currentHash.includes('code=') || currentHash.includes('error=') || currentHash.includes('access_token='));
  if (currentHash && !isAuthHash) {
    history.replaceState(null, '', window.location.pathname + window.location.search);
  }

  APP.msalApp = new msal.PublicClientApplication({
    auth: {
      clientId: CONFIG.msalClientId,
      authority: CONFIG.msalAuthority,
      redirectUri: window.location.origin + basePath,
      navigateToLoginRequestUrl: false,
    },
    cache: {
      cacheLocation: 'sessionStorage',
    },
  });

  try {
    await APP.msalApp.handleRedirectPromise();
  } catch (error) {
    showViewMessage('error', parseErrorPayload(error, 'Authentication initialization failed.'));
  }

  // Restore the routing hash after MSAL has finished processing.
  if (currentHash && !isAuthHash) {
    history.replaceState(null, '', window.location.pathname + window.location.search + currentHash);
  }

  const account = APP.msalApp.getActiveAccount?.() || APP.msalApp.getAllAccounts()[0] || null;
  if (account) {
    APP.account = account;
    APP.msalApp.setActiveAccount?.(account);
  }
  updateAuthUi();
}

async function login() {
  requireConfig('msalClientId', 'MSAL clientId');
  requireConfig('msalAuthority', 'MSAL authority');
  if (!APP.msalApp) {
    throw new Error('MSAL is not available.');
  }

  const result = await APP.msalApp.loginPopup({ scopes: STORAGE_SCOPES });
  APP.account = result.account || APP.msalApp.getAllAccounts()[0] || null;
  APP.msalApp.setActiveAccount?.(APP.account);
  updateAuthUi();
  return APP.account;
}

async function getToken() {
  if (!APP.account) {
    await login();
  }
  if (!APP.msalApp || !APP.account) {
    throw new Error('Sign in is required before calling Azure APIs.');
  }

  try {
    const result = await APP.msalApp.acquireTokenSilent({
      account: APP.account,
      scopes: STORAGE_SCOPES,
    });
    return result.accessToken;
  } catch {
    const result = await APP.msalApp.acquireTokenPopup({
      account: APP.account,
      scopes: STORAGE_SCOPES,
    });
    APP.account = result.account || APP.account;
    APP.msalApp.setActiveAccount?.(APP.account);
    updateAuthUi();
    return result.accessToken;
  }
}

async function getFunctionToken() {
  if (!APP.account) {
    await login();
  }
  if (!APP.msalApp || !APP.account) {
    throw new Error('Sign in is required before calling Azure APIs.');
  }

  const scopes = functionScopes();
  try {
    const result = await APP.msalApp.acquireTokenSilent({
      account: APP.account,
      scopes,
    });
    return result.accessToken;
  } catch {
    const result = await APP.msalApp.acquireTokenPopup({
      account: APP.account,
      scopes,
    });
    APP.account = result.account || APP.account;
    APP.msalApp.setActiveAccount?.(APP.account);
    updateAuthUi();
    return result.accessToken;
  }
}

function updateAuthUi() {
  if (!authControlsEl) {
    return;
  }

  if (APP.account) {
    authControlsEl.innerHTML = `
      <div class="kv small">
        <strong>${escapeHtml(APP.account.name || APP.account.username || 'Signed in')}</strong>
        <span class="muted">${escapeHtml(APP.account.username || '')}</span>
      </div>
    `;
    return;
  }

  const configMissing = !CONFIG.msalClientId || !CONFIG.msalAuthority;
  authControlsEl.innerHTML = configMissing
    ? '<span class="muted">Authentication is not configured.</span>'
    : '<button type="button" class="secondary" id="login-button">Sign in</button>';

  const button = document.getElementById('login-button');
  if (button) {
    button.addEventListener('click', async () => {
      try {
        await login();
        await renderActiveTab();
      } catch (error) {
        showViewMessage('error', parseErrorPayload(error, 'Sign-in failed.'));
        await renderActiveTab();
      }
    });
  }
}

function requireAuthenticatedView(title) {
  renderCard(title, '<p class="muted">Sign in to load this view.</p>');
}

// 3. Azure Tables API Helper
function tableBaseUrl(tableName) {
  requireConfig('storageAccount', 'Storage account');
  return `https://${CONFIG.storageAccount}.table.core.windows.net/${tableName}`;
}

function tableQueryUrl(tableName) {
  return `${tableBaseUrl(tableName)}()`;
}

function entityUrl(tableName, partitionKey, rowKey) {
  const encodedPartition = encodeURIComponent(String(partitionKey).replaceAll("'", "''"));
  const encodedRow = encodeURIComponent(String(rowKey).replaceAll("'", "''"));
  return `https://${CONFIG.storageAccount}.table.core.windows.net/${tableName}(PartitionKey='${encodedPartition}',RowKey='${encodedRow}')`;
}

async function fetchJson(url, options) {
  const response = await fetch(url, options);
  const text = await response.text();
  let payload = null;

  if (text) {
    try {
      payload = JSON.parse(text);
    } catch {
      payload = text;
    }
  }

  if (!response.ok) {
    throw new Error(parseErrorPayload(payload, `${response.status} ${response.statusText}`));
  }

  return payload;
}

async function queryTable(tableName, filter) {
  const token = await getToken();
  let allEntities = [];
  let nextPartitionKey = null;
  let nextRowKey = null;
  const maxPages = 10;

  for (let page = 0; page < maxPages; page++) {
    const url = new URL(tableQueryUrl(tableName));
    if (filter) url.searchParams.set('$filter', filter);
    if (nextPartitionKey) {
      url.searchParams.set('NextPartitionKey', nextPartitionKey);
      if (nextRowKey) url.searchParams.set('NextRowKey', nextRowKey);
    }

    const response = await fetch(url.toString(), {
      method: 'GET',
      headers: {
        Accept: 'application/json;odata=nometadata',
        Authorization: `Bearer ${token}`,
        'x-ms-version': '2019-02-02',
      },
    });

    if (!response.ok) {
      const text = await response.text();
      throw new Error(`Table query failed (${response.status}): ${text}`);
    }

    const payload = await response.json();
    if (Array.isArray(payload.value)) {
      allEntities = allEntities.concat(payload.value);
    }

    nextPartitionKey = response.headers.get('x-ms-continuation-NextPartitionKey');
    nextRowKey = response.headers.get('x-ms-continuation-NextRowKey');
    if (!nextPartitionKey) break;
  }

  return allEntities;
}

async function insertEntity(tableName, entity) {
  const token = await getToken();
  return fetchJson(tableBaseUrl(tableName), {
    method: 'POST',
    headers: {
      Accept: 'application/json;odata=nometadata',
      'Content-Type': 'application/json',
      Authorization: `Bearer ${token}`,
      'x-ms-version': '2019-02-02',
    },
    body: JSON.stringify(entity),
  });
}

async function upsertEntity(tableName, partitionKey, rowKey, entity) {
  const token = await getToken();
  return fetchJson(entityUrl(tableName, partitionKey, rowKey), {
    method: 'PUT',
    headers: {
      Accept: 'application/json;odata=nometadata',
      'Content-Type': 'application/json',
      Authorization: `Bearer ${token}`,
      'x-ms-version': '2019-02-02',
    },
    body: JSON.stringify(entity),
  });
}

async function listPrograms() {
  return sortByDateDesc(await queryTable(CONFIG.programsTable, "PartitionKey eq 'program'"), 'created_at');
}

// 4. Dashboard Tab
async function renderDashboard() {
  if (!APP.account) {
    requireAuthenticatedView('Dashboard');
    return;
  }

  renderCard('Dashboard', '<p class="muted">Loading dashboard…</p>');

  try {
    const [actualRows, desiredRows] = await Promise.all([
      queryTable(CONFIG.actualStateTable, ''),
      queryTable(CONFIG.desiredStateTable, ''),
    ]);

    const latestActual = latestByPartition(actualRows)
      .filter((row) => row.node_id)
      .sort((left, right) => String(left.node_id || '').localeCompare(String(right.node_id || '')));
    const desiredByPartition = new Map(latestByPartition(desiredRows).map((row) => [row.PartitionKey, row]));

    const rowsHtml = latestActual.map((actual) => {
      const desired = desiredByPartition.get(actual.PartitionKey);
      const desiredProgram = desired?.desired_assigned_program_hash || '';
      const actualProgram = actual.observed_current_program_hash || '';
      const desiredSchedule = desired?.desired_schedule_interval_s;
      const actualSchedule = actual.observed_schedule_interval_s;
      const diverged = (desired != null && desiredProgram !== actualProgram)
        || (desiredSchedule != null && desiredSchedule !== actualSchedule);
      const scheduleDisplay = desiredSchedule ?? actualSchedule ?? '—';
      const assignedProgram = desiredProgram || actual.observed_assigned_program_hash || '';
      const scheduleTitle = `Observed: ${actualSchedule ?? '—'} | Desired: ${desiredSchedule ?? '—'}`;
      return `
        <tr>
          <td>${escapeHtml(actual.node_id || '—')}</td>
          <td>${escapeHtml(actual.battery_mv ?? '—')}</td>
          <td>${escapeHtml(actual.firmware_version || '—')}</td>
          <td>${escapeHtml(actual.firmware_abi_version ?? '—')}</td>
          <td title="${escapeHtml(scheduleTitle)}">${escapeHtml(scheduleDisplay)}</td>
          <td>${formatHashCell(actualProgram)}</td>
          <td>${formatHashCell(assignedProgram)}</td>
          <td>${escapeHtml(relativeTime(actual.timestamp_ms))}</td>
          <td><span class="badge ${diverged ? 'warning' : 'success'}">${diverged ? 'Diverged' : 'Aligned'}</span></td>
          <td><button type="button" class="secondary dashboard-reboot-btn" data-node-id="${escapeHtml(actual.node_id || '')}">Reboot</button></td>
        </tr>
      `;
    }).join('');

    renderCard('Dashboard', `
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Node ID</th>
              <th>Battery (mV)</th>
              <th>Firmware</th>
              <th>ABI</th>
              <th>Schedule (s)</th>
              <th>Current Program</th>
              <th>Assigned Program</th>
              <th>Last Seen</th>
              <th>Status</th>
              <th>Actions</th>
            </tr>
          </thead>
          <tbody>${rowsHtml || '<tr><td colspan="10" class="muted">No node state found.</td></tr>'}</tbody>
        </table>
      </div>
    `);

    for (const button of contentEl.querySelectorAll('.dashboard-reboot-btn')) {
      button.addEventListener('click', async () => {
        const nodeId = String(button.dataset.nodeId || '');
        if (!nodeId || !confirm(`Reboot node ${nodeId}?`)) {
          return;
        }
        try {
          await sendAdminCommand('reboot_node', { node_id: nodeId });
          showViewMessage('success', `Reboot queued for ${nodeId}.`);
        } catch (error) {
          showViewMessage('error', parseErrorPayload(error, 'Failed to queue reboot.'));
        }
        await renderDashboard();
      });
    }
  } catch (error) {
    renderError('Dashboard', error);
  }

  setAutoRefresh(async () => {
    if (APP.activeTab === 'dashboard') {
      await renderDashboard();
    }
  });
}

// 5. Desired State Tab
let desiredRowKeySequence = 0;
function desiredRowKey(nowMs) {
  const seq = desiredRowKeySequence++;
  const invTs = (BigInt('0xffffffffffffffff') - BigInt(nowMs)).toString(16).padStart(16, '0');
  const invSeq = (BigInt('0xffffffffffffffff') - BigInt(seq)).toString(16).padStart(16, '0');
  return `${invTs}:${invSeq}:${randomHex(8)}`;
}

function desiredRowsTable(rows) {
  const sorted = latestByPartition(rows).sort((left, right) => String(left.node_id || '').localeCompare(String(right.node_id || '')));
  return `
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Node ID</th>
            <th>Schedule (s)</th>
            <th>Program Hash</th>
            <th>Updated</th>
          </tr>
        </thead>
        <tbody>
          ${sorted.map((row) => `
            <tr>
              <td>${escapeHtml(row.node_id || '—')}</td>
              <td>${escapeHtml(row.desired_schedule_interval_s ?? '—')}</td>
              <td>${formatHashCell(row.desired_assigned_program_hash || '')}</td>
              <td>${escapeHtml(relativeTime(row.timestamp_ms))}</td>
            </tr>
          `).join('') || '<tr><td colspan="4" class="muted">No desired state entries found.</td></tr>'}
        </tbody>
      </table>
    </div>
  `;
}

async function renderDesiredState() {
  if (!APP.account) {
    requireAuthenticatedView('Desired State');
    return;
  }

  const savedMessage = APP.viewMessage;
  renderCard('Desired State', '<p class="muted">Loading desired state…</p>');
  APP.viewMessage = savedMessage;

  try {
    const [programs, desiredRows, actualRows] = await Promise.all([
      listPrograms(),
      queryTable(CONFIG.desiredStateTable, ''),
      queryTable(CONFIG.actualStateTable, ''),
    ]);

    const latestActual = latestByPartition(actualRows)
      .filter((node) => node.node_id)
      .sort((left, right) => String(left.node_id || '').localeCompare(String(right.node_id || '')));
    const desiredByPartition = new Map(
      latestByPartition(desiredRows).map((row) => [row.PartitionKey, row]),
    );

    const nodeOptions = [
      '<option value="" disabled selected>Select a node…</option>',
      ...latestActual.map((node) =>
        `<option value="${escapeHtml(node.node_id || '')}">${escapeHtml(node.node_id || '—')}</option>`),
    ].join('');

    const programOptions = [
      '<option value="">No program target</option>',
      ...programs.map((program) => `<option value="${escapeHtml(program.RowKey)}">${escapeHtml(truncHash(program.RowKey))} — ${escapeHtml(program.source_filename || 'unnamed')}</option>`),
    ].join('');
    const ephemeralProgramOptions = [
      '<option value="">None</option>',
      ...programs
        .filter((program) => program.verification_profile === 'ephemeral')
        .map((program) => `<option value="${escapeHtml(program.RowKey)}">${escapeHtml(truncHash(program.RowKey))} — ${escapeHtml(program.source_filename || 'unnamed')}</option>`),
    ].join('');

    renderCard('Desired State', `
      <div class="panel stack">
        <form id="desired-state-form" class="form-grid">
          <label>Node ID
            <select name="nodeId" required>${nodeOptions}</select>
          </label>
          <label>Schedule Interval (s)
            <input name="scheduleInterval" type="number" min="1" step="1" placeholder="60">
          </label>
          <label>Program Hash
            <select name="programHash">${programOptions}</select>
          </label>
          <label>Ephemeral Program (optional)
            <select name="ephemeralProgramHash">${ephemeralProgramOptions}</select>
          </label>
          <div>
            <button type="submit" class="primary">Save Desired State</button>
          </div>
        </form>
      </div>
      <div class="panel stack">
        <h2>Latest Desired State</h2>
        ${desiredRowsTable(desiredRows)}
      </div>
    `);

    const form = document.getElementById('desired-state-form');

    // Auto-populate fields when a node is selected (WEB-0206, WEB-0207)
    const nodeSelect = form?.querySelector('[name="nodeId"]');
    nodeSelect?.addEventListener('change', () => {
      const selectedNodeId = nodeSelect.value;
      if (!selectedNodeId) {
        return;
      }

      const actualNode = latestActual.find((node) => node.node_id === selectedNodeId);
      const desiredNode = desiredByPartition.get(actualNode?.PartitionKey);

      // Per-field desired-over-actual fallback: use the desired value for
      // each field when present, otherwise fall back to the latest actual
      // value. We use ?? (not ||) so that a zero schedule or an explicit
      // empty-string hash from a future schema change won't be skipped.
      const scheduleValue = desiredNode?.desired_schedule_interval_s
        ?? actualNode?.observed_schedule_interval_s
        ?? '';
      const hashValue = (desiredNode?.desired_assigned_program_hash
        ?? actualNode?.observed_assigned_program_hash
        ?? '').toLowerCase();
      const ephemeralHashValue = String(desiredNode?.desired_ephemeral_program_hash || '').toLowerCase();

      const scheduleInput = form.querySelector('[name="scheduleInterval"]');
      if (scheduleInput) {
        scheduleInput.value = scheduleValue;
      }

      const programSelect = form.querySelector('[name="programHash"]');
      if (programSelect) {
        const matchingOption = [...programSelect.options].find(
          (opt) => opt.value.toLowerCase() === hashValue,
        );
        programSelect.value = matchingOption ? matchingOption.value : '';
      }

      const ephemeralSelect = form.querySelector('[name="ephemeralProgramHash"]');
      if (ephemeralSelect) {
        const matchingOption = [...ephemeralSelect.options].find(
          (opt) => opt.value.toLowerCase() === ephemeralHashValue,
        );
        ephemeralSelect.value = matchingOption ? matchingOption.value : '';
      }
    });

    form?.addEventListener('submit', async (event) => {
      event.preventDefault();
      const formData = new FormData(form);
      const nodeId = String(formData.get('nodeId') || '').trim();
      const scheduleValue = String(formData.get('scheduleInterval') || '').trim();
      const programHash = String(formData.get('programHash') || '').trim();
      const ephemeralProgramHash = String(formData.get('ephemeralProgramHash') || '').trim();

      if (!nodeId) {
        showViewMessage('error', 'Node ID is required.');
        await renderDesiredState();
        return;
      }

      try {
        const nowMs = Date.now();
        const partitionKey = `n:${await sha256hex(nodeId)}`;
        const rowKey = desiredRowKey(nowMs);
        const entity = {
          PartitionKey: partitionKey,
          RowKey: rowKey,
          node_id: nodeId,
          timestamp_ms: String(nowMs),
          'timestamp_ms@odata.type': 'Edm.Int64',
        };

        if (scheduleValue) {
          entity.desired_schedule_interval_s = Number(scheduleValue);
          entity['desired_schedule_interval_s@odata.type'] = 'Edm.Int32';
        }
        if (programHash) {
          entity.desired_assigned_program_hash = programHash.toLowerCase();
        }
        if (ephemeralProgramHash) {
          entity.desired_ephemeral_program_hash = ephemeralProgramHash.toLowerCase();
        }

        await insertEntity(CONFIG.desiredStateTable, entity);
        showViewMessage('success', 'Desired state saved.');
      } catch (error) {
        showViewMessage('error', parseErrorPayload(error, 'Failed to save desired state.'));
      }

      await renderDesiredState();
    });
  } catch (error) {
    renderError('Desired State', error);
  }
}

// 6. Programs Tab
function programRowsTable(programs) {
  return `
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Hash</th>
            <th>Filename</th>
            <th>ABI</th>
            <th>Size</th>
            <th>Created</th>
            <th>Actions</th>
          </tr>
        </thead>
        <tbody>
          ${programs.map((program) => `
            <tr>
              <td>${formatHashCell(program.RowKey)}</td>
              <td>${escapeHtml(program.source_filename || '—')}</td>
              <td>${escapeHtml(program.abi_version ?? '—')}</td>
              <td>${escapeHtml(program.size_bytes ?? '—')}</td>
              <td>${escapeHtml(program.created_at || '—')}</td>
              <td><button type="button" class="secondary program-delete-btn" data-hash="${escapeHtml(program.RowKey)}" style="color:var(--danger)">🗑️</button></td>
            </tr>
          `).join('') || '<tr><td colspan="6" class="muted">No programs found.</td></tr>'}
        </tbody>
      </table>
    </div>
  `;
}

async function renderPrograms() {
  if (!APP.account) {
    requireAuthenticatedView('Programs');
    return;
  }

  const savedMessage = APP.viewMessage;
  renderCard('Programs', '<p class="muted">Loading programs…</p>');
  APP.viewMessage = savedMessage;

  try {
    const [programs, desiredRows] = await Promise.all([
      listPrograms(),
      queryTable(CONFIG.desiredStateTable, ''),
    ]);

    renderCard('Programs', `
      <div class="panel stack">
        <form id="program-upload-form" class="form-grid">
          <label>ELF File
            <input name="elf" type="file" accept=".o,.elf" required>
          </label>
          <label>Source Filename
            <input name="sourceFilename" type="text" required>
          </label>
          <label>ABI Version
            <input name="abiVersion" type="number" min="1" step="1" value="2" required>
          </label>
          <label>Verification Profile
            <select name="verificationProfile">
              <option value="resident">resident</option>
              <option value="ephemeral">ephemeral</option>
            </select>
          </label>
          <div>
            <button type="submit" class="primary">Upload Program</button>
          </div>
        </form>
      </div>
      <div class="panel stack">
        <h2>Programs</h2>
        ${programRowsTable(programs)}
      </div>
    `);

    const latestDesired = latestByPartition(desiredRows);
    for (const button of contentEl.querySelectorAll('.program-delete-btn')) {
      button.addEventListener('click', async () => {
        const hash = String(button.dataset.hash || '');
        if (!hash) {
          return;
        }
        const matchingDesiredRows = latestDesired.filter(
          (row) => String(row.desired_assigned_program_hash || '').toLowerCase() === hash.toLowerCase(),
        );
        const warning = matchingDesiredRows.length > 0
          ? `

⚠ This program is assigned to ${matchingDesiredRows.length} node(s). Deleting it will not unassign them.`
          : '';
        if (!confirm(`Delete program ${truncHash(hash)}?${warning}`)) {
          return;
        }
        try {
          await deleteProgramFromFunction(hash);
          showViewMessage('success', `Program deleted: ${truncHash(hash)}`);
        } catch (error) {
          showViewMessage('error', parseErrorPayload(error, 'Failed to delete program.'));
        }
        await renderPrograms();
      });
    }

    const form = document.getElementById('program-upload-form');
    const fileInput = form?.querySelector('input[name="elf"]');
    const nameInput = form?.querySelector('input[name="sourceFilename"]');

    fileInput?.addEventListener('change', () => {
      const file = fileInput.files?.[0];
      if (file && nameInput && !nameInput.value) {
        nameInput.value = file.name;
      } else if (file && nameInput) {
        nameInput.value = file.name;
      }
    });

    form?.addEventListener('submit', async (event) => {
      event.preventDefault();
      const formData = new FormData(form);
      const file = fileInput?.files?.[0];
      if (!file) {
        showViewMessage('error', 'Select an ELF file to upload.');
        await renderPrograms();
        return;
      }

      try {
        requireConfig('functionAppName', 'Function app name');
        const token = await getFunctionToken();
        const arrayBuf = await file.arrayBuffer();
        const bytes = new Uint8Array(arrayBuf);
        const chunkSize = 8192;
        const chunks = [];
        for (let i = 0; i < bytes.length; i += chunkSize) {
          chunks.push(String.fromCharCode.apply(null, bytes.subarray(i, i + chunkSize)));
        }
        const elfBase64 = btoa(chunks.join(''));

        const payload = {
          elf: elfBase64,
          source_filename: String(formData.get('sourceFilename') || file.name),
          abi_version: Number(formData.get('abiVersion') || 2),
          verification_profile: String(formData.get('verificationProfile') || 'resident'),
        };

        const response = await fetch(`https://${CONFIG.functionAppName}.azurewebsites.net/api/programs/ingest`, {
          method: 'POST',
          headers: {
            Authorization: `Bearer ${token}`,
            'Content-Type': 'application/json',
          },
          body: JSON.stringify(payload),
        });

        const responseText = await response.text();
        let result = null;
        if (responseText) {
          try {
            result = JSON.parse(responseText);
          } catch {
            result = responseText;
          }
        }
        if (!response.ok) {
          throw new Error(parseErrorPayload(result, 'Program ingest failed.'));
        }

        const programHash = result && typeof result === 'object' ? result.program_hash : '';
        showViewMessage('success', `Program uploaded: ${programHash || 'ok'}`);
      } catch (error) {
        showViewMessage('error', parseErrorPayload(error, 'Program ingest failed.'));
      }

      await renderPrograms();
    });
  } catch (error) {
    renderError('Programs', error);
  }
}

// 8. Sensor Data Tab (WEB-0700)

// Series display overrides persisted in localStorage.
// Shape: { [seriesKey]: { displayName, scaleDivisor, unitSuffix } }
const SERIES_OVERRIDES_KEY = 'sonde_series_overrides';

function loadSeriesOverrides() {
  try {
    const raw = localStorage.getItem(SERIES_OVERRIDES_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) return {};
    return parsed;
  } catch { return {}; }
}

function saveSeriesOverrides(overrides) {
  try {
    localStorage.setItem(SERIES_OVERRIDES_KEY, JSON.stringify(overrides));
  } catch {
    // Storage disabled or quota exceeded — surface to caller via return value
    return false;
  }
  return true;
}

function getSeriesDisplayLabel(series, overrides) {
  const ov = overrides || loadSeriesOverrides();
  const o = ov[series.key];
  return (o && o.displayName) ? o.displayName : series.label;
}

function getSeriesScale(seriesKey, overrides) {
  const ov = overrides || loadSeriesOverrides();
  const o = ov[seriesKey];
  if (o && typeof o.scaleDivisor === 'number' && Number.isFinite(o.scaleDivisor) && o.scaleDivisor !== 0) {
    return o.scaleDivisor;
  }
  return null;
}

function getSeriesUnitSuffix(seriesKey, overrides) {
  const ov = overrides || loadSeriesOverrides();
  const o = ov[seriesKey];
  return (o && o.unitSuffix) ? o.unitSuffix : '';
}

const SENSOR_STATE = {
  timeRange: '24h',
  viewMode: 'graph',
  selectedSeries: new Set(),
  seriesInitialized: false,
  autoRefresh: false,
};

const TIME_RANGE_MS = {
  '1h': 60 * 60 * 1000,
  '24h': 24 * 60 * 60 * 1000,
  '7d': 7 * 24 * 60 * 60 * 1000,
};

function reverseTimestampHex(ms) {
  const max = BigInt('0xffffffffffffffff');
  return (max - BigInt(ms)).toString(16).padStart(16, '0');
}

async function querySensorData(partitionKeys, timeRangeMs) {
  const token = await getToken();
  const now = Date.now();
  const start = now - timeRangeMs;
  const rkStart = reverseTimestampHex(now);
  const rkEnd = reverseTimestampHex(start);

  const fetchPartition = async (pk) => {
    const filter = `PartitionKey eq '${pk}' and RowKey ge '${rkStart}' and RowKey le '${rkEnd}~'`;
    const url = new URL(tableQueryUrl(CONFIG.sensorDataTable));
    url.searchParams.set('$filter', filter);
    url.searchParams.set('$top', '1000');

    const response = await fetch(url.toString(), {
      method: 'GET',
      headers: {
        Accept: 'application/json;odata=nometadata',
        Authorization: `Bearer ${token}`,
        'x-ms-version': '2019-02-02',
      },
    });

    if (!response.ok) {
      const text = await response.text();
      throw new Error(`SensorData query failed (${response.status}): ${text}`);
    }

    const payload = await response.json();
    return Array.isArray(payload.value) ? payload.value : [];
  };

  const allEntities = [];
  const batchSize = 6;
  for (let i = 0; i < partitionKeys.length; i += batchSize) {
    const batch = partitionKeys.slice(i, i + batchSize);
    const results = await Promise.all(batch.map(fetchPartition));
    for (const entities of results) {
      allEntities.push(...entities);
    }
  }
  return allEntities;
}

function parseSensorReadings(decodedReadings) {
  if (!decodedReadings || decodedReadings === '') {
    return null;
  }
  try {
    return JSON.parse(decodedReadings);
  } catch {
    return null;
  }
}

function toPlottableNumber(value) {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === 'string') {
    const num = Number(value);
    if (Number.isFinite(num) && Math.abs(num) <= Number.MAX_SAFE_INTEGER) {
      return num;
    }
  }
  return null;
}

function formatReadingValue(value) {
  if (typeof value === 'string') {
    return value;
  }
  if (typeof value === 'number') {
    return String(value);
  }
  return '—';
}

function extractSeries(rows, nodeIdMap) {
  const seriesMap = new Map();

  for (const row of rows) {
    const readings = parseSensorReadings(row.decoded_readings);
    if (!readings) continue;

    const nodeId = nodeIdMap.get(row.PartitionKey) || row.PartitionKey;
    const programHash = row.program_hash || '';
    const timestampMs = Number(row.timestamp_ms);
    if (!Number.isFinite(timestampMs)) continue;

    for (const [readingName, value] of Object.entries(readings)) {
      const key = `${row.PartitionKey}|${programHash}|${readingName}`;
      if (!seriesMap.has(key)) {
        seriesMap.set(key, {
          key,
          nodeId,
          programHash,
          readingName,
          label: `${truncHash(nodeId)} / ${truncHash(programHash)} / ${readingName}`,
          points: [],
        });
      }
      const plottable = toPlottableNumber(value);
      if (plottable !== null) {
        seriesMap.get(key).points.push({ x: timestampMs, y: plottable });
      }
    }
  }

  for (const series of seriesMap.values()) {
    series.points.sort((a, b) => a.x - b.x);
  }

  return [...seriesMap.values()];
}

function downsamplePoints(points, maxPoints) {
  if (points.length <= maxPoints) return points;
  const step = points.length / (maxPoints - 1);
  const result = [];
  for (let i = 0; i < maxPoints - 1; i++) {
    result.push(points[Math.floor(i * step)]);
  }
  result.push(points[points.length - 1]);
  return result;
}

const CHART_COLORS = [
  '#2f6fed', '#e74c3c', '#27ae60', '#f39c12', '#8e44ad',
  '#1abc9c', '#d35400', '#2c3e50', '#c0392b', '#16a085',
  '#e67e22', '#9b59b6', '#3498db', '#2ecc71', '#e74c3c',
  '#f1c40f', '#1abc9c', '#e91e63', '#00bcd4', '#ff9800',
];

function renderSensorChart(allSeries) {
  const selected = allSeries.filter((s) => SENSOR_STATE.selectedSeries.has(s.key));

  if (APP.sensorChart) {
    APP.sensorChart.destroy();
    APP.sensorChart = null;
  }

  if (selected.length === 0) {
    const chartArea = contentEl.querySelector('.sensor-chart-area');
    if (chartArea) {
      const plottableCount = allSeries.filter((s) => s.points.length > 0).length;
      let message;
      if (allSeries.length === 0) {
        message = 'No decoded sensor readings found for the selected time range.';
      } else if (plottableCount === 0) {
        message = 'All readings contain non-numeric values that cannot be plotted. Switch to table view to inspect the data.';
      } else {
        message = 'No series selected. Use the series picker above to select data to plot.';
      }
      chartArea.innerHTML = `<p class="muted">${message}</p>`;
    }
    return;
  }

  const chartArea = contentEl.querySelector('.sensor-chart-area');
  if (!chartArea) return;
  chartArea.innerHTML = '<canvas id="sensor-canvas"></canvas>';

  const canvas = document.getElementById('sensor-canvas');
  if (!canvas || typeof Chart === 'undefined') {
    chartArea.innerHTML = '<p class="alert error">Chart.js is not available. Switch to table view.</p>';
    return;
  }

  const overrides = loadSeriesOverrides();

  const datasets = selected.slice(0, 20).map((series, i) => {
    const divisor = getSeriesScale(series.key, overrides);
    const scaledPoints = downsamplePoints(series.points, 500).map((p) => ({
      x: p.x,
      y: divisor ? p.y / divisor : p.y,
    }));
    const suffix = getSeriesUnitSuffix(series.key, overrides);
    return {
      label: getSeriesDisplayLabel(series, overrides),
      nodeId: series.nodeId,
      programHash: series.programHash,
      readingName: series.readingName,
      seriesKey: series.key,
      unitSuffix: suffix,
      data: scaledPoints,
      borderColor: CHART_COLORS[i % CHART_COLORS.length],
      backgroundColor: 'transparent',
      borderWidth: 1.5,
      pointRadius: series.points.length > 100 ? 0 : 2,
      tension: 0.1,
    };
  });

  APP.sensorChart = new Chart(canvas, {
    type: 'line',
    data: { datasets },
    options: {
      responsive: true,
      maintainAspectRatio: false,
      interaction: { mode: 'nearest', intersect: false },
      scales: {
        x: {
          type: 'linear',
          title: { display: true, text: 'Time' },
          ticks: {
            callback(value) {
              const d = new Date(value);
              const hh = d.getHours().toString().padStart(2, '0');
              const mm = d.getMinutes().toString().padStart(2, '0');
              if (SENSOR_STATE.timeRange === '7d') {
                return `${d.getMonth() + 1}/${d.getDate()} ${hh}:${mm}`;
              }
              return `${hh}:${mm}`;
            },
            maxTicksLimit: 12,
          },
        },
        y: {
          title: {
            display: true,
            text: (() => {
              const suffixes = [...new Set(datasets.map((d) => d.unitSuffix))];
              return suffixes.length === 1 && suffixes[0] ? `Value (${suffixes[0]})` : 'Value';
            })(),
          },
        },
      },
      plugins: {
        tooltip: {
          callbacks: {
            title(items) {
              if (!items.length) return '';
              return new Date(items[0].parsed.x).toLocaleString();
            },
            label(item) {
              const ds = item.dataset;
              const suffix = ds.unitSuffix || '';
              return `${ds.label}: ${item.parsed.y}${suffix}`;
            },
          },
        },
        legend: {
          position: 'bottom',
          labels: { boxWidth: 12, padding: 8 },
        },
      },
    },
  });
}

function renderSensorTable(rows, nodeIdMap) {
  const sorted = [...rows].sort((a, b) => {
    const ta = Number(a.timestamp_ms) || 0;
    const tb = Number(b.timestamp_ms) || 0;
    return tb - ta;
  });

  const rowsHtml = sorted.map((row) => {
    const ts = Number(row.timestamp_ms);
    const timeStr = Number.isFinite(ts) ? new Date(ts).toLocaleString() : '—';
    const nodeId = nodeIdMap.get(row.PartitionKey) || row.PartitionKey;
    const readings = parseSensorReadings(row.decoded_readings);
    let readingsDisplay = '—';
    if (readings) {
      readingsDisplay = Object.entries(readings)
        .map(([k, v]) => `${escapeHtml(k)}: ${escapeHtml(formatReadingValue(v))}`)
        .join(', ');
    }
    const rawPayload = row.raw_payload || '—';
    const truncatedRaw = rawPayload.length > 40 ? rawPayload.slice(0, 40) + '…' : rawPayload;

    return `
      <tr>
        <td>${escapeHtml(timeStr)}</td>
        <td>${escapeHtml(nodeId)}</td>
        <td>${formatHashCell(row.program_hash)}</td>
        <td>${readingsDisplay}</td>
        <td><code title="${escapeHtml(rawPayload)}">${escapeHtml(truncatedRaw)}</code></td>
      </tr>
    `;
  }).join('');

  return `
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Timestamp</th>
            <th>Node ID</th>
            <th>Program Hash</th>
            <th>Decoded Readings</th>
            <th>Raw Payload</th>
          </tr>
        </thead>
        <tbody>${rowsHtml || '<tr><td colspan="5" class="muted">No sensor data found.</td></tr>'}</tbody>
      </table>
    </div>
  `;
}

function showSeriesEditDialog(seriesKey, rawLabel) {
  // Remove any existing dialog
  const existing = document.getElementById('series-edit-dialog');
  if (existing) existing.remove();

  const overrides = loadSeriesOverrides();
  const current = overrides[seriesKey] || {};
  const safeDivisor = (typeof current.scaleDivisor === 'number' && Number.isFinite(current.scaleDivisor))
    ? current.scaleDivisor : '';

  const dialog = document.createElement('div');
  dialog.id = 'series-edit-dialog';
  dialog.className = 'series-edit-overlay';
  dialog.setAttribute('role', 'dialog');
  dialog.setAttribute('aria-modal', 'true');
  dialog.setAttribute('aria-label', 'Edit series display settings');
  dialog.innerHTML = `
    <div class="series-edit-panel panel">
      <h3>Edit Series Display</h3>
      <p class="muted small">Raw label: ${escapeHtml(rawLabel)}</p>
      <div class="stack">
        <label>
          Display Name
          <input type="text" id="series-edit-name" placeholder="${escapeHtml(rawLabel)}"
                 value="${escapeHtml(current.displayName || '')}">
        </label>
        <label>
          Scale Divisor
          <input type="number" id="series-edit-divisor" step="any" placeholder="1"
                 value="${safeDivisor}">
          <span class="muted small">e.g. 1000 to convert milli-units → units</span>
        </label>
        <label>
          Unit Suffix
          <input type="text" id="series-edit-unit" placeholder=""
                 value="${escapeHtml(current.unitSuffix || '')}">
          <span class="muted small">e.g. °C, %, hPa — appended to values</span>
        </label>
        <div style="display:flex;gap:0.5rem;justify-content:flex-end">
          <button type="button" class="secondary" id="series-edit-reset">Reset to Default</button>
          <button type="button" class="secondary" id="series-edit-cancel">Cancel</button>
          <button type="button" class="primary" id="series-edit-save">Save</button>
        </div>
      </div>
    </div>
  `;

  document.body.appendChild(dialog);

  const previousFocus = document.activeElement;
  const nameInput = document.getElementById('series-edit-name');
  if (nameInput) nameInput.focus();

  function closeDialog() {
    dialog.remove();
    if (previousFocus && typeof previousFocus.focus === 'function') {
      previousFocus.focus();
    }
  }

  // Focus trap: cycle through focusable elements within the dialog
  dialog.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      closeDialog();
      return;
    }
    if (e.key !== 'Tab') return;
    const focusable = dialog.querySelectorAll('input, button, [tabindex]:not([tabindex="-1"])');
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  });

  dialog.addEventListener('click', (e) => {
    if (e.target === dialog) closeDialog();
  });

  document.getElementById('series-edit-cancel').addEventListener('click', () => {
    closeDialog();
  });

  document.getElementById('series-edit-reset').addEventListener('click', async () => {
    const ov = loadSeriesOverrides();
    delete ov[seriesKey];
    if (!saveSeriesOverrides(ov)) {
      alert('Failed to save settings — browser storage may be full or disabled.');
      return;
    }
    closeDialog();
    await renderSensorData();
  });

  document.getElementById('series-edit-save').addEventListener('click', async () => {
    const ov = loadSeriesOverrides();
    const name = document.getElementById('series-edit-name').value.trim();
    const divisorStr = document.getElementById('series-edit-divisor').value.trim();
    const unit = document.getElementById('series-edit-unit').value.trim();

    const divisor = divisorStr ? Number(divisorStr) : 0;

    if (divisorStr && (!Number.isFinite(divisor) || divisor === 0)) {
      const divisorInput = document.getElementById('series-edit-divisor');
      if (divisorInput) divisorInput.focus();
      alert('Scale divisor must be a finite non-zero number.');
      return;
    }

    if (name || (divisor && divisor !== 0) || unit) {
      ov[seriesKey] = {
        displayName: name || '',
        scaleDivisor: (divisor && Number.isFinite(divisor) && divisor !== 0) ? divisor : 0,
        unitSuffix: unit || '',
      };
    } else {
      delete ov[seriesKey];
    }

    if (!saveSeriesOverrides(ov)) {
      alert('Failed to save settings — browser storage may be full or disabled.');
      return;
    }
    closeDialog();
    await renderSensorData();
  });
}

async function renderSensorData() {
  if (!APP.account) {
    requireAuthenticatedView('Sensor Data');
    return;
  }

  renderCard('Sensor Data', '<p class="muted">Loading sensor data…</p>');

  if (APP.sensorChart) {
    APP.sensorChart.destroy();
    APP.sensorChart = null;
  }

  try {
    const actualRows = await queryTable(CONFIG.actualStateTable, '');
    const latestActual = latestByPartition(actualRows).sort((a, b) =>
      String(a.node_id || '').localeCompare(String(b.node_id || ''))
    );

    const nodeIdMap = new Map(latestActual.map((r) => [r.PartitionKey, r.node_id]));
    const partitionKeys = latestActual.map((r) => r.PartitionKey);

    if (partitionKeys.length === 0) {
      renderCard('Sensor Data', '<p class="muted">No nodes have reported state yet.</p>');
      if (SENSOR_STATE.autoRefresh) {
        setAutoRefresh(async () => {
          if (APP.activeTab === 'sensor-data') {
            await renderSensorData();
          }
        });
      }
      return;
    }

    const rangeMs = TIME_RANGE_MS[SENSOR_STATE.timeRange] || TIME_RANGE_MS['24h'];
    const sensorRows = await querySensorData(partitionKeys, rangeMs);
    const allSeries = extractSeries(sensorRows, nodeIdMap);

    // Prune stale and non-plottable selections before auto-selection
    const currentPlottableKeys = new Set(
      allSeries.filter((s) => s.points.length > 0).map((s) => s.key)
    );
    const sizeBefore = SENSOR_STATE.selectedSeries.size;
    for (const key of [...SENSOR_STATE.selectedSeries]) {
      if (!currentPlottableKeys.has(key)) {
        SENSOR_STATE.selectedSeries.delete(key);
      }
    }
    const prunedCount = sizeBefore - SENSOR_STATE.selectedSeries.size;
    if (SENSOR_STATE.selectedSeries.size === 0 && prunedCount > 0) {
      SENSOR_STATE.seriesInitialized = false;
    }

    if (!SENSOR_STATE.seriesInitialized && currentPlottableKeys.size > 0) {
      SENSOR_STATE.seriesInitialized = true;
      const plottable = allSeries.filter((s) => s.points.length > 0);
      for (const s of plottable.slice(0, Math.min(plottable.length, 5))) {
        SENSOR_STATE.selectedSeries.add(s.key);
      }
    }

    const timeRangeButtons = Object.keys(TIME_RANGE_MS).map((range) => {
      const active = SENSOR_STATE.timeRange === range ? ' active' : '';
      return `<button type="button" class="secondary sensor-range-btn${active}" data-range="${range}">${escapeHtml(range)}</button>`;
    }).join('');

    const viewToggle = `
      <button type="button" class="secondary sensor-view-btn${SENSOR_STATE.viewMode === 'graph' ? ' active' : ''}" data-view="graph">Graph</button>
      <button type="button" class="secondary sensor-view-btn${SENSOR_STATE.viewMode === 'table' ? ' active' : ''}" data-view="table">Table</button>
    `;

    const pickerOverrides = loadSeriesOverrides();
    const seriesCheckboxes = allSeries.map((s) => {
      const checked = SENSOR_STATE.selectedSeries.has(s.key) ? ' checked' : '';
      const plottable = s.points.length > 0;
      const suffix = plottable ? '' : ' <span class="muted">(no numeric data)</span>';
      const displayLabel = getSeriesDisplayLabel(s, pickerOverrides);
      const hasOverride = displayLabel !== s.label;
      const overrideTitle = hasOverride ? ` title="Raw: ${escapeHtml(s.label)}"` : '';
      const ariaLabel = `Edit display settings for ${displayLabel}`;
      return `<span class="sensor-series-item"><label class="sensor-series-label"${overrideTitle}><input type="checkbox" value="${escapeHtml(s.key)}"${checked}${plottable ? '' : ' disabled'}> ${escapeHtml(displayLabel)}${suffix}</label><button type="button" class="sensor-series-edit-btn" data-series-key="${escapeHtml(s.key)}" data-series-label="${escapeHtml(s.label)}" title="Edit display settings" aria-label="${escapeHtml(ariaLabel)}">✏️</button></span>`;
    }).join('');

    const autoRefreshChecked = SENSOR_STATE.autoRefresh ? ' checked' : '';

    renderCard('Sensor Data', `
      <div class="panel sensor-controls">
        <div class="sensor-control-row">
          <span class="sensor-control-group">
            <strong>Time range:</strong> ${timeRangeButtons}
          </span>
          <span class="sensor-control-group">
            <strong>View:</strong> ${viewToggle}
          </span>
          <label class="sensor-control-group">
            <input type="checkbox" id="sensor-auto-refresh"${autoRefreshChecked}> Auto-refresh
          </label>
        </div>
        ${allSeries.length > 0 ? `
          <details class="sensor-series-picker" open>
            <summary><strong>Series</strong> (${allSeries.length} available, max 20 plotted)</summary>
            <div class="sensor-series-grid">${seriesCheckboxes}</div>
          </details>
        ` : ''}
      </div>
      <div class="panel">
        ${SENSOR_STATE.viewMode === 'graph'
          ? '<div class="sensor-chart-area chart-container"><p class="muted">Rendering chart…</p></div>'
          : renderSensorTable(sensorRows, nodeIdMap)}
      </div>
    `);

    if (SENSOR_STATE.viewMode === 'graph') {
      renderSensorChart(allSeries);
    }

    // Attach event handlers
    for (const btn of contentEl.querySelectorAll('.sensor-range-btn')) {
      btn.addEventListener('click', async () => {
        SENSOR_STATE.timeRange = btn.dataset.range;
        await renderSensorData();
      });
    }

    for (const btn of contentEl.querySelectorAll('.sensor-view-btn')) {
      btn.addEventListener('click', async () => {
        SENSOR_STATE.viewMode = btn.dataset.view;
        await renderSensorData();
      });
    }

    const seriesCheckboxEls = contentEl.querySelectorAll('.sensor-series-grid input[type="checkbox"]');
    for (const cb of seriesCheckboxEls) {
      cb.addEventListener('change', () => {
        if (cb.checked) {
          if (SENSOR_STATE.selectedSeries.size >= 20) {
            cb.checked = false;
            return;
          }
          SENSOR_STATE.selectedSeries.add(cb.value);
        } else {
          SENSOR_STATE.selectedSeries.delete(cb.value);
        }
        if (SENSOR_STATE.viewMode === 'graph') {
          renderSensorChart(allSeries);
        }
      });
    }

    for (const btn of contentEl.querySelectorAll('.sensor-series-edit-btn')) {
      btn.addEventListener('click', () => {
        const seriesKey = btn.dataset.seriesKey;
        const rawLabel = btn.dataset.seriesLabel;
        showSeriesEditDialog(seriesKey, rawLabel);
      });
    }

    const autoRefreshCb = document.getElementById('sensor-auto-refresh');
    if (autoRefreshCb) {
      autoRefreshCb.addEventListener('change', () => {
        SENSOR_STATE.autoRefresh = autoRefreshCb.checked;
        if (SENSOR_STATE.autoRefresh) {
          setAutoRefresh(async () => {
            if (APP.activeTab === 'sensor-data') {
              await renderSensorData();
            }
          });
        } else {
          clearRefresh();
        }
      });
    }

    if (SENSOR_STATE.autoRefresh) {
      setAutoRefresh(async () => {
        if (APP.activeTab === 'sensor-data') {
          await renderSensorData();
        }
      });
    }
  } catch (error) {
    renderError('Sensor Data', error);
    if (SENSOR_STATE.autoRefresh) {
      setAutoRefresh(async () => {
        if (APP.activeTab === 'sensor-data') {
          await renderSensorData();
        }
      });
    }
  }
}

// 10. Gateway Tab (WEB-1000)
async function renderGateway() {
  if (!APP.account) {
    requireAuthenticatedView('Gateway');
    return;
  }

  const savedMessage = APP.viewMessage;
  renderCard('Gateway', '<p class="muted">Loading gateway status…</p>');
  APP.viewMessage = savedMessage;

  try {
    const [gwStatusRows, pubkeyEntity, stateEntity, saltEntity] = await Promise.all([
      queryTable(CONFIG.actualStateTable, "PartitionKey eq 'gw:status'"),
      getEntity(CONFIG.gatewayEscrowTable, 'gateway', 'pubkey').catch(() => null),
      getEntity(CONFIG.gatewayEscrowTable, 'gateway', 'state').catch(() => null),
      getEntity(CONFIG.gatewayEscrowTable, 'gateway', 'salt').catch(() => null),
    ]);

    const gwStatus = gwStatusRows.length > 0
      ? gwStatusRows.reduce((left, right) => String(left.RowKey) < String(right.RowKey) ? left : right)
      : null;

    const modemConnected = gwStatus?.modem_connected;
    const modemChannel = Number(gwStatus?.modem_channel);
    const modemMac = gwStatus?.modem_mac;

    const modemStatusHtml = gwStatus
      ? `<div class="kv-grid">
          <div class="kv"><strong>Connection</strong> <span class="badge ${modemConnected ? 'success' : 'error'}">${modemConnected ? 'Connected' : 'Disconnected'}</span></div>
          <div class="kv"><strong>WiFi Channel</strong> <span>${escapeHtml(Number.isFinite(modemChannel) ? modemChannel : '—')}</span></div>
          <div class="kv"><strong>MAC Address</strong> <code>${escapeHtml(modemMac || '—')}</code></div>
        </div>`
      : '<p class="muted">No modem data available.</p>';

    let scanHtml = '';
    if (gwStatus?.scan_results) {
      try {
        const results = typeof gwStatus.scan_results === 'string'
          ? JSON.parse(gwStatus.scan_results)
          : gwStatus.scan_results;
        if (Array.isArray(results) && results.length > 0) {
          const scanTime = gwStatus.scan_timestamp ? relativeTime(gwStatus.scan_timestamp) : '';
          scanHtml = `
            <h3>Scan Results ${scanTime ? `<span class="muted small">(${escapeHtml(scanTime)})</span>` : ''}</h3>
            <div class="table-wrap"><table>
              <thead><tr><th>Channel</th><th>RSSI</th><th>SSID</th></tr></thead>
              <tbody>${results.map((result) => `<tr><td>${escapeHtml(result.channel ?? result[1] ?? '')}</td><td>${escapeHtml(result.rssi ?? result[2] ?? '')}</td><td>${escapeHtml(result.ssid ?? result[3] ?? '')}</td></tr>`).join('')}</tbody>
            </table></div>`;
        }
      } catch {
        // Ignore malformed scan results.
      }
    }

    const channelOptions = Array.from({ length: 14 }, (_, index) => index + 1)
      .map((channel) => `<option value="${channel}" ${channel === modemChannel ? 'selected' : ''}>${channel}</option>`)
      .join('');

    let fingerprintHtml = '';
    if (pubkeyEntity?.public_key) {
      try {
        const pubkeyBytes = base64ToBytes(pubkeyEntity.public_key);
        const words = await computeFingerprint(pubkeyBytes);
        fingerprintHtml = `
          <div class="kv"><strong>Fingerprint</strong> <code class="fingerprint">${words.map(escapeHtml).join(' ')}</code></div>
          <div class="kv"><strong>Key Epoch</strong> <span>${escapeHtml(pubkeyEntity.key_epoch ?? '—')}</span></div>
          <div class="kv"><strong>Published</strong> <span>${escapeHtml(pubkeyEntity.created_at ? relativeTime(pubkeyEntity.created_at) : '—')}</span></div>`;
      } catch (error) {
        fingerprintHtml = `<p class="muted">Error computing fingerprint: ${escapeHtml(error.message)}</p>`;
      }
    } else {
      fingerprintHtml = '<p class="muted">No recovery key published.</p>';
    }

    const escrowState = stateEntity?.escrow_state || null;
    const keyVersion = stateEntity?.escrow_key_version;
    const stateBadgeClass = escrowState === 'ready'
      ? 'success'
      : (escrowState === 'bootstrapping' || escrowState === 'rotation_in_progress')
        ? 'warning'
        : escrowState === 'degraded'
          ? 'error'
          : 'muted';
    const stateDisplay = escrowState || 'Unknown';

    let kdfHtml = '<span class="muted">No KDF salt configured.</span>';
    if (saltEntity?.kdf_params_json) {
      try {
        const kdf = typeof saltEntity.kdf_params_json === 'string'
          ? JSON.parse(saltEntity.kdf_params_json)
          : saltEntity.kdf_params_json;
        kdfHtml = `<span>Argon2id (m=${escapeHtml(kdf.m_cost)}, t=${escapeHtml(kdf.t_cost)}, p=${escapeHtml(kdf.p_cost)})</span>`;
      } catch {
        kdfHtml = '<span class="muted">Invalid KDF parameters.</span>';
      }
    }

    const escrowWarning = escrowState && escrowState !== 'ready'
      ? `<div class="alert warning">⚠ Key escrow is not ready (${escapeHtml(escrowState)}). Recovery may not be possible.</div>`
      : '';
    const canRotate = Boolean(pubkeyEntity?.public_key && saltEntity?.salt);

    renderCard('Gateway', `
      <div class="panel stack">
        <h2>Modem</h2>
        ${modemStatusHtml}
        <form id="channel-form" class="form-grid" style="margin-top:1rem">
          <label>WiFi Channel
            <select name="channel">
              <option value="" disabled ${Number.isFinite(modemChannel) ? '' : 'selected'}>Select channel…</option>
              ${channelOptions}
            </select>
          </label>
          <div style="display:flex;gap:0.5rem">
            <button type="submit" class="primary">Set Channel</button>
            <button type="button" class="secondary" id="scan-btn">Scan Channels</button>
          </div>
        </form>
        ${scanHtml}
      </div>

      <div class="panel stack">
        <h2>Key Escrow</h2>
        ${escrowWarning}
        <div class="kv-grid">
          ${fingerprintHtml}
          <div class="kv"><strong>Escrow State</strong> <span class="badge ${stateBadgeClass}">${escapeHtml(stateDisplay)}</span></div>
          <div class="kv"><strong>Key Version</strong> <span>${escapeHtml(keyVersion ?? '—')}</span></div>
          <div class="kv"><strong>KDF</strong> ${kdfHtml}</div>
        </div>
        <div style="margin-top:1rem">
          <button type="button" class="primary" id="rotate-key-btn" ${canRotate ? '' : 'disabled title="Recovery key or KDF salt not available"'}>Rotate Key</button>
        </div>
      </div>
    `);

    document.getElementById('channel-form')?.addEventListener('submit', async (event) => {
      event.preventDefault();
      const channel = Number(new FormData(event.target).get('channel'));
      if (!channel || channel < 1 || channel > 14) {
        showViewMessage('error', 'Select a valid channel (1–14).');
        await renderGateway();
        return;
      }
      try {
        await sendAdminCommand('set_channel', { channel });
        showViewMessage('success', 'Channel change requested.');
      } catch (error) {
        showViewMessage('error', parseErrorPayload(error, 'Failed to set channel.'));
      }
      await renderGateway();
    });

    document.getElementById('scan-btn')?.addEventListener('click', async () => {
      try {
        await sendAdminCommand('scan_channels', {});
        showViewMessage('success', 'Scan requested — results will appear on refresh.');
      } catch (error) {
        showViewMessage('error', parseErrorPayload(error, 'Failed to request scan.'));
      }
      await renderGateway();
    });

    document.getElementById('rotate-key-btn')?.addEventListener('click', () => {
      showRotationWizard(pubkeyEntity, stateEntity, saltEntity);
    });

    setAutoRefresh(async () => {
      if (APP.activeTab === 'gateway') {
        await renderGateway();
      }
    });
  } catch (error) {
    renderError('Gateway', error);
  }
}

function showRotationWizard(pubkeyEntity, stateEntity, saltEntity) {
  const overlayHtml = `<div class="env-manager-overlay" id="rotation-overlay" role="dialog" aria-modal="true" aria-label="Key Rotation">
    <div class="env-manager-panel panel" style="max-width:480px">
      <h2>Rotate Master Key</h2>
      <div id="rotation-step-1">
        <p>Verify this fingerprint matches your gateway's OLED display:</p>
        <div id="rotation-fingerprint" class="muted">Computing…</div>
        <p class="small muted">Key epoch: ${escapeHtml(pubkeyEntity?.key_epoch ?? '—')}</p>
        <label style="display:flex;gap:0.5rem;align-items:center;margin-top:0.75rem">
          <input type="checkbox" id="fingerprint-verified">
          I have verified the fingerprint
        </label>
        <div style="margin-top:1rem;display:flex;gap:0.5rem">
          <button type="button" class="secondary" id="rotation-cancel-1">Cancel</button>
          <button type="button" class="primary" id="rotation-next" disabled>Next →</button>
        </div>
      </div>
      <div id="rotation-step-2" style="display:none">
        <label>Passphrase (min 12 characters)
          <input type="password" id="rotation-passphrase" minlength="12" autocomplete="off">
        </label>
        <label>Confirm passphrase
          <input type="password" id="rotation-passphrase-confirm" autocomplete="off">
        </label>
        <div id="rotation-pass-error" class="alert error" style="display:none;margin-top:0.5rem"></div>
        <div style="margin-top:1rem;display:flex;gap:0.5rem">
          <button type="button" class="secondary" id="rotation-back">← Back</button>
          <button type="button" class="primary" id="rotation-submit">Rotate Key</button>
        </div>
      </div>
      <div id="rotation-step-3" style="display:none">
        <p id="rotation-status" class="muted">Processing…</p>
        <div class="spinner" style="margin:1rem 0"></div>
      </div>
      <div id="rotation-step-4" style="display:none">
        <div id="rotation-result"></div>
        <div style="margin-top:1rem;display:flex;gap:0.5rem">
          <button type="button" class="primary" id="rotation-close">Close</button>
          <button type="button" class="secondary" id="rotation-retry" style="display:none">Retry</button>
        </div>
      </div>
    </div>
  </div>`;

  let overlay = document.getElementById('rotation-overlay');
  if (overlay) {
    overlay.remove();
  }
  document.body.insertAdjacentHTML('beforeend', overlayHtml);

  (async () => {
    try {
      const pubkeyBytes = base64ToBytes(pubkeyEntity.public_key);
      const words = await computeFingerprint(pubkeyBytes);
      document.getElementById('rotation-fingerprint').innerHTML =
        `<code class="fingerprint" style="font-size:1.2em">${words.map(escapeHtml).join(' ')}</code>`;
    } catch (error) {
      document.getElementById('rotation-fingerprint').textContent = `Error: ${error.message}`;
    }
  })();

  const checkbox = document.getElementById('fingerprint-verified');
  const nextBtn = document.getElementById('rotation-next');
  checkbox?.addEventListener('change', () => {
    nextBtn.disabled = !checkbox.checked;
  });

  document.getElementById('rotation-cancel-1')?.addEventListener('click', () => {
    document.getElementById('rotation-overlay')?.remove();
  });

  nextBtn?.addEventListener('click', () => {
    document.getElementById('rotation-step-1').style.display = 'none';
    document.getElementById('rotation-step-2').style.display = '';
    document.getElementById('rotation-passphrase')?.focus();
  });

  document.getElementById('rotation-back')?.addEventListener('click', () => {
    document.getElementById('rotation-step-2').style.display = 'none';
    document.getElementById('rotation-step-1').style.display = '';
  });

  document.getElementById('rotation-submit')?.addEventListener('click', async () => {
    const passphrase = document.getElementById('rotation-passphrase')?.value || '';
    const confirmPassphrase = document.getElementById('rotation-passphrase-confirm')?.value || '';
    const errorEl = document.getElementById('rotation-pass-error');

    if (passphrase.length < 12) {
      if (errorEl) {
        errorEl.textContent = 'Passphrase must be at least 12 characters.';
        errorEl.style.display = '';
      }
      return;
    }
    if (passphrase !== confirmPassphrase) {
      if (errorEl) {
        errorEl.textContent = 'Passphrases do not match.';
        errorEl.style.display = '';
      }
      return;
    }
    if (errorEl) {
      errorEl.style.display = 'none';
    }

    document.getElementById('rotation-step-2').style.display = 'none';
    document.getElementById('rotation-step-3').style.display = '';

    try {
      await performKeyRotation(passphrase, pubkeyEntity, stateEntity, saltEntity);
      document.getElementById('rotation-step-3').style.display = 'none';
      document.getElementById('rotation-step-4').style.display = '';
      document.getElementById('rotation-result').innerHTML =
        '<div class="alert success">✓ Key rotation initiated. The gateway will process the rotation on the next cycle.</div>';
    } catch (error) {
      document.getElementById('rotation-step-3').style.display = 'none';
      document.getElementById('rotation-step-4').style.display = '';
      document.getElementById('rotation-result').innerHTML =
        `<div class="alert error">✗ Key rotation failed: ${escapeHtml(error.message)}</div>`;
      const retryBtn = document.getElementById('rotation-retry');
      if (retryBtn) {
        retryBtn.style.display = '';
      }
    }
  });

  document.getElementById('rotation-close')?.addEventListener('click', () => {
    document.getElementById('rotation-overlay')?.remove();
    renderGateway().catch((error) => renderError('Gateway', error));
  });
  document.getElementById('rotation-retry')?.addEventListener('click', () => {
    document.getElementById('rotation-step-4').style.display = 'none';
    document.getElementById('rotation-step-2').style.display = '';
    document.getElementById('rotation-passphrase').value = '';
    document.getElementById('rotation-passphrase-confirm').value = '';
    document.getElementById('rotation-pass-error').style.display = 'none';
  });
}

async function performKeyRotation(passphrase, pubkeyEntity, stateEntity, saltEntity) {
  const statusEl = document.getElementById('rotation-status');
  const pubkey = base64ToBytes(pubkeyEntity.public_key);
  const salt = base64ToBytes(saltEntity.salt);
  const keyEpoch = Number(pubkeyEntity.key_epoch);

  if (pubkey.length !== 32) {
    throw new Error('Recovery public key must be 32 bytes.');
  }
  if (salt.length === 0) {
    throw new Error('KDF salt is missing.');
  }
  if (!Number.isFinite(keyEpoch) || keyEpoch < 0) {
    throw new Error('Key epoch is invalid.');
  }

  let kdfParams = { m_cost: 65536, t_cost: 3, p_cost: 1 };
  if (saltEntity.kdf_params_json) {
    try {
      const parsed = typeof saltEntity.kdf_params_json === 'string'
        ? JSON.parse(saltEntity.kdf_params_json)
        : saltEntity.kdf_params_json;
      kdfParams = {
        m_cost: parsed.m_cost || 65536,
        t_cost: parsed.t_cost || 3,
        p_cost: parsed.p_cost || 1,
      };
    } catch {
      // Use default KDF parameters.
    }
  }

  if (statusEl) {
    statusEl.textContent = 'Deriving key with Argon2id…';
  }
  if (typeof argon2 === 'undefined') {
    throw new Error('Argon2 library not loaded.');
  }
  const argonResult = await argon2.hash({
    pass: passphrase,
    salt,
    type: argon2.ArgonType.Argon2id,
    mem: kdfParams.m_cost,
    time: kdfParams.t_cost,
    parallelism: kdfParams.p_cost,
    hashLen: 32,
  });
  const masterKey = argonResult.hash;

  if (statusEl) {
    statusEl.textContent = 'Generating ephemeral keypair…';
  }
  if (typeof nacl === 'undefined') {
    throw new Error('TweetNaCl library not loaded.');
  }
  const ephemeral = nacl.box.keyPair();

  const sharedSecret = nacl.scalarMult(ephemeral.secretKey, pubkey);
  const operationId = crypto.getRandomValues(new Uint8Array(16));

  if (statusEl) {
    statusEl.textContent = 'Encrypting master key…';
  }
  const epochBuf = new ArrayBuffer(8);
  const epochView = new DataView(epochBuf);
  epochView.setBigUint64(0, BigInt(keyEpoch), false);
  const targetEpochBe = new Uint8Array(epochBuf);
  const info = concatBytes(targetEpochBe, operationId);
  const hkdfKey = await hkdfSha256(sharedSecret, 'sonde-escrow-v1', info, 32);

  const nonce = crypto.getRandomValues(new Uint8Array(12));
  const aad = concatBytes(operationId, targetEpochBe);
  const importedKey = await crypto.subtle.importKey('raw', hkdfKey, 'AES-GCM', false, ['encrypt']);
  const ciphertext = await crypto.subtle.encrypt(
    { name: 'AES-GCM', iv: nonce, additionalData: aad, tagLength: 128 },
    importedKey,
    masterKey,
  );
  const ctBytes = new Uint8Array(ciphertext);
  const encryptedMasterKey = ctBytes.slice(0, ctBytes.length - 16);
  const tag = ctBytes.slice(ctBytes.length - 16);

  masterKey.fill(0);
  ephemeral.secretKey.fill(0);
  sharedSecret.fill(0);

  if (statusEl) {
    statusEl.textContent = 'Sending to gateway…';
  }
  const rotationCounter = stateEntity?.escrow_key_version != null
    ? Number(stateEntity.escrow_key_version) + 1
    : 1;
  const payload = {
    target_key_epoch: keyEpoch,
    sender_public_key: bytesToBase64(ephemeral.publicKey),
    encrypted_master_key: bytesToBase64(encryptedMasterKey),
    nonce: bytesToBase64(nonce),
    tag: bytesToBase64(tag),
    operation_id: bytesToBase64(operationId),
    rotation_counter: rotationCounter,
    expiry_ms: Date.now() + 300000,
  };

  requireConfig('functionAppName', 'Function app name');
  const token = await getFunctionToken();
  const response = await fetch(
    `https://${CONFIG.functionAppName}.azurewebsites.net/api/keys/rotate`,
    {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${token}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(payload),
    },
  );
  if (!response.ok) {
    const text = await response.text();
    throw new Error(parseErrorPayload(text, `Rotation failed (${response.status}).`));
  }
}

// 9. Tab Router
function setActiveTab(tabId) {
  APP.activeTab = TAB_IDS.includes(tabId) ? tabId : 'dashboard';
  for (const button of document.querySelectorAll('.tab-button')) {
    button.classList.toggle('active', button.dataset.tab === APP.activeTab);
  }
}

async function renderActiveTab() {
  clearRefresh();
  if (APP.sensorChart) {
    APP.sensorChart.destroy();
    APP.sensorChart = null;
  }

  switch (APP.activeTab) {
    case 'desired-state':
      await renderDesiredState();
      break;
    case 'programs':
      await renderPrograms();
      break;
    case 'sensor-data':
      await renderSensorData();
      break;
    case 'gateway':
      await renderGateway();
      break;
    case 'dashboard':
    default:
      await renderDashboard();
      break;
  }
}

function attachTabHandlers() {
  for (const button of document.querySelectorAll('.tab-button')) {
    button.addEventListener('click', () => {
      const nextTab = button.dataset.tab || 'dashboard';
      setActiveTab(nextTab);
      renderActiveTab().catch((error) => renderError('Navigation failed', error));
    });
  }
}

async function init() {
  attachTabHandlers();
  document.getElementById('env-gear-btn')?.addEventListener('click', () => showEnvironmentManager());
  const env = loadActiveEnvironment();
  if (!env) {
    showEnvironmentManager();
    return;
  }
  updateEnvironmentIndicator();
  await initMsal();
  setActiveTab('dashboard');
  await renderActiveTab();
}

function clearMsalSessionStorage() {
  // Only remove MSAL-related keys to avoid clearing unrelated session data
  // on shared origins (e.g. GitHub Pages project sites).
  try {
    const keysToRemove = [];
    for (let i = 0; i < sessionStorage.length; i++) {
      const key = sessionStorage.key(i);
      if (key && (key.startsWith('msal.') || key.includes('.login.') || key.includes('.acquireToken.'))) {
        keysToRemove.push(key);
      }
    }
    for (const key of keysToRemove) {
      sessionStorage.removeItem(key);
    }
  } catch {
    // sessionStorage may be unavailable.
  }
}

async function switchEnvironment(name) {
  clearRefresh();
  setActiveEnvironmentName(name);
  const envs = loadEnvironments();
  const env = envs.find((e) => e.name === name);
  applyEnvironment(env);
  APP.msalApp = null;
  APP.account = null;
  clearMsalSessionStorage();
  updateEnvironmentIndicator();
  await initMsal();
  await renderActiveTab();
}

function updateEnvironmentIndicator() {
  const el = document.getElementById('env-indicator');
  if (!el) return;
  const name = getActiveEnvironmentName();
  el.textContent = name || '';
  el.title = name ? `Active environment: ${name}` : 'No environment selected';
}

function showEnvironmentManager() {
  const envs = loadEnvironments();
  const activeName = getActiveEnvironmentName();

  const envListHtml = envs.length === 0
    ? '<p class="muted">No environments configured. Add one to get started.</p>'
    : `<div class="table-wrap"><table>
        <thead><tr><th>Name</th><th>Storage Account</th><th>Function App</th><th></th></tr></thead>
        <tbody>${envs.map((env) => `<tr>
          <td><strong>${escapeHtml(env.name)}</strong>${env.name === activeName ? ' <span class="badge success">active</span>' : ''}</td>
          <td><code>${escapeHtml(env.storageAccount || '')}</code></td>
          <td><code>${escapeHtml(env.functionAppName || '')}</code></td>
          <td style="white-space:nowrap">
            ${env.name !== activeName ? `<button type="button" class="secondary env-use-btn" data-env="${escapeHtml(env.name)}">Use</button> ` : ''}
            <button type="button" class="secondary env-edit-btn" data-env="${escapeHtml(env.name)}">Edit</button>
            <button type="button" class="secondary env-delete-btn" data-env="${escapeHtml(env.name)}" style="color:var(--danger)">Delete</button>
          </td>
        </tr>`).join('')}
        </tbody></table></div>`;

  const overlayHtml = `<div class="env-manager-overlay" id="env-manager-overlay" role="dialog" aria-modal="true" aria-label="Environment Manager">
    <div class="env-manager-panel panel">
      <h2>Environments</h2>
      ${envListHtml}
      <div style="margin-top:1rem;display:flex;gap:0.5rem;flex-wrap:wrap">
        <button type="button" class="primary" id="env-add-btn">Add Environment</button>
        ${envs.length > 0 ? '<button type="button" class="secondary" id="env-close-btn">Close</button>' : ''}
      </div>
    </div>
  </div>`;

  let overlay = document.getElementById('env-manager-overlay');
  if (overlay) overlay.remove();
  document.body.insertAdjacentHTML('beforeend', overlayHtml);

  document.getElementById('env-add-btn')?.addEventListener('click', () => showEnvironmentForm(null));
  document.getElementById('env-close-btn')?.addEventListener('click', () => {
    document.getElementById('env-manager-overlay')?.remove();
  });

  for (const btn of document.querySelectorAll('.env-use-btn')) {
    btn.addEventListener('click', () => {
      document.getElementById('env-manager-overlay')?.remove();
      switchEnvironment(btn.dataset.env).catch((error) => renderError('Switch failed', error));
    });
  }
  for (const btn of document.querySelectorAll('.env-edit-btn')) {
    btn.addEventListener('click', () => {
      const env = loadEnvironments().find((e) => e.name === btn.dataset.env);
      if (env) showEnvironmentForm(env);
    });
  }
  for (const btn of document.querySelectorAll('.env-delete-btn')) {
    btn.addEventListener('click', () => {
      const name = btn.dataset.env;
      const envsList = loadEnvironments().filter((e) => e.name !== name);
      if (!saveEnvironments(envsList)) {
        showViewMessage('error', 'Failed to save changes. Browser storage may be disabled or full.');
      }
      if (getActiveEnvironmentName() === name) {
        if (envsList.length > 0) {
          switchEnvironment(envsList[0].name).catch((error) => renderError('Switch failed', error));
        } else {
          clearRefresh();
          setActiveEnvironmentName('');
          CONFIG.msalClientId = '';
          CONFIG.msalAuthority = '';
          CONFIG.storageAccount = '';
          CONFIG.functionAppName = '';
          APP.msalApp = null;
          APP.account = null;
          clearMsalSessionStorage();
          updateEnvironmentIndicator();
          updateAuthUi();
          contentEl.innerHTML = '';
        }
      }
      showEnvironmentManager();
    });
  }
}

function showEnvironmentForm(existingEnv) {
  const isEdit = existingEnv != null;
  const title = isEdit ? 'Edit Environment' : 'Add Environment';

  const formHtml = `<div class="env-manager-overlay" id="env-form-overlay" role="dialog" aria-modal="true" aria-label="${title}">
    <div class="env-manager-panel panel">
      <h2>${title}</h2>
      <div class="stack">
        <label>Name <input type="text" id="env-field-name" value="${escapeHtml(existingEnv?.name || '')}" ${isEdit ? 'readonly' : ''} placeholder="e.g. production"></label>
        <label>Entra Client ID <input type="text" id="env-field-clientId" value="${escapeHtml(existingEnv?.clientId || '')}" placeholder="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"></label>
        <label>Entra Tenant ID <input type="text" id="env-field-tenantId" value="${escapeHtml(existingEnv?.tenantId || '')}" placeholder="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"></label>
        <label>Storage Account <input type="text" id="env-field-storageAccount" value="${escapeHtml(existingEnv?.storageAccount || '')}" placeholder="mystorageaccount"></label>
        <label>Function App Name <input type="text" id="env-field-functionAppName" value="${escapeHtml(existingEnv?.functionAppName || '')}" placeholder="sonde-decoder-xxxx"></label>
      </div>
      <div style="margin-top:1rem;display:flex;gap:0.5rem">
        <button type="button" class="primary" id="env-save-btn">Save</button>
        <button type="button" class="secondary" id="env-cancel-btn">Cancel</button>
      </div>
      <div id="env-form-error" class="alert error" style="display:none;margin-top:0.75rem"></div>
    </div>
  </div>`;

  let formOverlay = document.getElementById('env-form-overlay');
  if (formOverlay) formOverlay.remove();
  document.body.insertAdjacentHTML('beforeend', formHtml);

  document.getElementById('env-cancel-btn')?.addEventListener('click', () => {
    document.getElementById('env-form-overlay')?.remove();
  });

  document.getElementById('env-save-btn')?.addEventListener('click', () => {
    const name = document.getElementById('env-field-name')?.value.trim();
    const clientId = document.getElementById('env-field-clientId')?.value.trim();
    const tenantId = document.getElementById('env-field-tenantId')?.value.trim();
    const storageAccount = document.getElementById('env-field-storageAccount')?.value.trim();
    const functionAppName = document.getElementById('env-field-functionAppName')?.value.trim();
    const errorEl = document.getElementById('env-form-error');

    if (!name || !clientId || !tenantId || !storageAccount || !functionAppName) {
      if (errorEl) {
        errorEl.textContent = 'All fields are required.';
        errorEl.style.display = '';
      }
      return;
    }

    const guidPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
    if (!guidPattern.test(clientId)) {
      if (errorEl) { errorEl.textContent = 'Client ID must be a valid GUID.'; errorEl.style.display = ''; }
      return;
    }
    if (!guidPattern.test(tenantId)) {
      if (errorEl) { errorEl.textContent = 'Tenant ID must be a valid GUID.'; errorEl.style.display = ''; }
      return;
    }
    if (!/^[a-z0-9]{3,24}$/.test(storageAccount)) {
      if (errorEl) { errorEl.textContent = 'Storage Account must be 3–24 lowercase alphanumeric characters.'; errorEl.style.display = ''; }
      return;
    }
    if (!/^[a-zA-Z0-9][a-zA-Z0-9-]{0,58}[a-zA-Z0-9]$/.test(functionAppName)) {
      if (errorEl) { errorEl.textContent = 'Function App Name must be 2–60 alphanumeric characters with optional hyphens.'; errorEl.style.display = ''; }
      return;
    }

    const envs = loadEnvironments();
    if (!isEdit && envs.some((e) => e.name === name)) {
      if (errorEl) {
        errorEl.textContent = `An environment named "${name}" already exists.`;
        errorEl.style.display = '';
      }
      return;
    }

    const envData = { name, clientId, tenantId, storageAccount, functionAppName };
    if (isEdit) {
      const idx = envs.findIndex((e) => e.name === name);
      if (idx >= 0) envs[idx] = envData;
    } else {
      envs.push(envData);
    }
    if (!saveEnvironments(envs)) {
      if (errorEl) { errorEl.textContent = 'Failed to save environment. Browser storage may be disabled or full.'; errorEl.style.display = ''; }
      return;
    }

    const isFirstEnv = !isEdit && envs.length === 1;
    const isActiveEnv = getActiveEnvironmentName() === name;

    document.getElementById('env-form-overlay')?.remove();

    if (isFirstEnv || isActiveEnv) {
      document.getElementById('env-manager-overlay')?.remove();
      switchEnvironment(name).catch((error) => renderError('Switch failed', error));
    } else {
      showEnvironmentManager();
    }
  });
}

document.addEventListener('DOMContentLoaded', () => {
  // MSAL loginPopup() opens a popup that loads this SPA.  The popup only needs
  // MSAL to process the auth response — skip full app init to avoid unnecessary
  // API calls and rendering.
  if (window.opener && window.opener !== window) {
    return;
  }
  init().catch((error) => renderError('Application failed to start', error));
});
