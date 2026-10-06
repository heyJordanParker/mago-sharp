use crate::binding::Local;

/// Where one PHP# method's locals start in each stack of `LocalScopes`.
#[derive(Debug, Clone, Copy)]
struct Frame {
    open: usize,
    blocks: usize,
    closed: usize,
}

/// The locals the binder can see, one frame per method being walked.
///
/// A method never sees the locals of the method or file around it. The frames share the stacks, so a method allocates
/// nothing once the stacks have grown to fit one.
#[derive(Debug)]
pub struct LocalScopes<'arena> {
    /// The locals of the open blocks, innermost last.
    open: Vec<(&'arena [u8], Local)>,
    /// Where each open block starts in `open`, innermost last. A frame's first block holds the parameters.
    blocks: Vec<usize>,
    /// The locals of blocks that have already closed.
    closed: Vec<(&'arena [u8], Local)>,
    frames: Vec<Frame>,
}

impl<'arena> LocalScopes<'arena> {
    pub fn enter_method(&mut self) {
        self.frames.push(Frame { open: self.open.len(), blocks: self.blocks.len(), closed: self.closed.len() });
        self.blocks.push(self.open.len());
    }

    pub fn exit_method(&mut self) {
        if let Some(frame) = self.frames.pop() {
            self.open.truncate(frame.open);
            self.blocks.truncate(frame.blocks);
            self.closed.truncate(frame.closed);
        }
    }

    pub fn enter_block(&mut self) {
        self.frame();
        self.blocks.push(self.open.len());
    }

    pub fn exit_block(&mut self) {
        let frame = self.frame();
        if self.blocks.len() > frame.blocks
            && let Some(start) = self.blocks.pop()
        {
            self.closed.extend(self.open.drain(start..));
        }
    }

    /// Declares a local in the innermost open block.
    ///
    /// Returns the local an open block already declares under the same name, if any.
    pub fn declare(&mut self, name: &'arena [u8], local: Local) -> Option<Local> {
        let earlier = self.lookup(name);
        if self.blocks.len() > self.frame().blocks {
            self.open.push((name, local));
        }

        earlier
    }

    /// Declares a local that is not in scope where it is declared, such as a pattern's variable, which comes into
    /// scope only where its test holds.
    ///
    /// Returns the local an open block already declares under the same name, if any.
    pub fn declare_out_of_scope(&mut self, name: &'arena [u8], local: Local) -> Option<Local> {
        let earlier = self.lookup(name);
        self.frame();
        self.closed.push((name, local));

        earlier
    }

    /// Returns the local an open block declares under `name`, innermost first.
    pub fn lookup(&self, name: &[u8]) -> Option<Local> {
        let frame = self.frames.last()?;

        self.open[frame.open..]
            .iter()
            .rev()
            .find_map(|(declared, local)| if *declared == name { Some(*local) } else { None })
    }

    /// Returns the latest local of this method declared under `name` whose block has closed.
    pub fn lookup_closed(&self, name: &[u8]) -> Option<Local> {
        let frame = self.frames.last()?;

        self.closed[frame.closed..]
            .iter()
            .rev()
            .find_map(|(declared, local)| if *declared == name { Some(*local) } else { None })
    }

    fn frame(&mut self) -> Frame {
        if self.frames.is_empty() {
            self.enter_method();
        }

        self.frames[self.frames.len() - 1]
    }
}

impl Default for LocalScopes<'_> {
    fn default() -> Self {
        let mut scopes = Self { open: Vec::new(), blocks: Vec::new(), closed: Vec::new(), frames: Vec::new() };
        scopes.enter_method();
        scopes
    }
}
