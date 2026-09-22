use zz_frontend::lexer::lex;
use zz_frontend::token::{Trivia, TriviaKind};
fn main() {
    let src = "// header\nx := 1 // trailing\n/// doc line\ny := 2\n";
    let lexed = lex(src);
    for (i, t) in lexed.tokens.iter().enumerate() {
        println!("{:2}: {:?} {:?} leading: {:?}", i, t.kind, t.text, t.leading);
    }
}
