// Shapes of what `oxi headless` sends; see src/headless/mod.rs (`message_json`, `config_json`,
// `git_state_json`, `quota_json`).

export type ThinkingBlock = {type: 'thinking'; text: string};
export type AnswerBlock = {type: 'answer'; text: string};
export type ToolStatus = 'pending' | 'in_progress' | 'completed' | 'failed' | 'interrupted';
export type ToolBlock = {
	type: 'tool';
	id: string | null;
	name: string;
	kind: string;
	title: string;
	status: ToolStatus;
	args: string | null;
	output: string;
	output_truncated: boolean;
	diff: string | null;
	diff_truncated: boolean;
	full_output_path: string | null;
	locations: {path: string; line: number | null}[];
};
export type Block = ThinkingBlock | AnswerBlock | ToolBlock;

export type Route = {
	provider: string;
	label: string;
	model: string;
	reason: string;
	failover_from: string | null;
	direct: boolean;
};

export type UserMessage = {
	role: 'user';
	text: string;
	summary: boolean;
	attachments: string[];
	images: {index: number; mime: string}[];
};
export type AssistantMessage = {
	role: 'assistant';
	streaming: boolean;
	blocks: Block[];
	evidence: any;
	route: Route | null;
	worked_secs: number | null;
};
export type Message = UserMessage | AssistantMessage;

export type ChatConfig = {provider: string; label: string; model: string; effort: string};

export type Approval = {tool: string; summary: string; allow_prefix: string | null};

export type ChatSummary = {
	id: string;
	title: string;
	modified: number;
	provider: string | null;
	running: boolean;
	approval: boolean;
};

export type Provider = {id: string; label: string; description: string; acp: boolean; model: string};

export type GitEntry = {path: string; status: string};
export type GitState = {
	repo: boolean;
	branch: string;
	branches: string[];
	ahead: number;
	behind: number;
	staged: GitEntry[];
	unstaged: GitEntry[];
	log: {hash: string; date: string; author: string; message: string}[];
	diff: {title: string; text: string} | null;
	error: string | null;
};

export type QuotaWindow = {label: string; used_pct: number; resets_in: number | null; scope: string | null};
export type Quota = {
	provider: string;
	supported: boolean;
	enabled: boolean;
	windows: QuotaWindow[];
	plan?: string | null;
	credits_left?: number | null;
	limit_reached?: boolean;
	error?: string | null;
};

export type Attachment =
	| {kind: 'image'; mime: string; data: string; name: string}
	| {kind: 'text'; name: string; text: string};

export type Workspace = {path: string; name: string; modified: number | null; chats: number};
