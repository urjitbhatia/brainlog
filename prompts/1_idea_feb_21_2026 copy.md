---
name: seed idea
description: This is the very initial seed idea prompt.
---

Let's plan to implement brainlog. A command line wrapper for running other
executables similar to `time`. However, in this case, it transparently captures
standard out, standard error, and standard in for the process it's running. It
does not get in the way of anything else, including signals and so on. It
transparently forwards all signals, all networking state like ports, etc. File
descriptors, everything.

It's just a wrapper that runs another executable. However, it captures and
stores the standard air, standard out, and standard in. Then the idea is to
wrap it in an MCP server. I want to run my web backend, API backend, etc. for
development projects locally via brainlog. The Brainlog will capture all of
its input/output standard error standout.

And then the MCP server will let Claude or any other add-on agent ask for logs
for the service. Also, let the MCP filter by tags as well as ports and file
descriptors. So the interface to the MCP will look like cloud asking for "give
me services that are running on port X", Give me services that are running the
executable [npm/go/etc], Give me services that are named "Y", and so on for
"discovery".

Another tool we want to then support is given a unique service ID or handle
which BrainLog will generate. Give me the logs for that service and the LLM
could specify if it wants unified logs which would unify standard err, standard
out or they can selectively ask for which type of log as well as ask for
standard in. And the tool can also directly provide the logs given the service
description.

An llm can say, "Do you have a service that is running the web frontend for the
Pimlico project?" In which case our MCP will run its own internal agent, look
at the service description it has, and then figure out from the context what
the service is running and whether that matches. Go through this and ask me
clarifying questions until you are in a spot where you can start implementing.
This will be implemented in Rust.
