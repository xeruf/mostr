use std::collections::HashMap;

fn main() {
    let mut map: HashMap<usize, String> = HashMap::new();
    let add_string = |map: &mut HashMap<usize, String>, string: String| {
        map.insert(string.len(), string);
    };
    add_string(&mut map, "hi".to_string());
    add_string(&mut map, "ho".to_string());
    map.add_string("hi".to_string());
    map.add_string("ho".to_string());
    map.get(&1);
}

trait InsertString {
    fn add_string(&mut self, event: String);
}
impl InsertString for HashMap<usize, String> {
    fn add_string(&mut self, event: String) {
        self.insert(event.len(), event);
    }
}
