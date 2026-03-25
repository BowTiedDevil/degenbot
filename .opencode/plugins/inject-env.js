export const InjectEnvPlugin = async () => {
  return {
    "shell.env": async (input, output) => {
      output.env.DEGENBOT_DEBUG = "true"
      output.env.DEGENBOT_PROGRESS_BAR = "false"
      output.env.DEGENBOT_ONE_CHUNK = "true"
      output.env.DEGENBOT_COVERAGE = "true"
      //      output.env.DEGENBOT_CHUNK_SIZE = "1"
      output.env.DEGENBOT_VERIFY_ALL = "true"
      output.env.DEGENBOT_VERIFY_BLOCK = "true"
      output.env.DEGENBOT_VERIFY_CHUNK = "true"
      output.env.OPENCODE_EXPERIMENTAL_BASH_DEFAULT_TIMEOUT_MS = 1800000
    },
  }
}
