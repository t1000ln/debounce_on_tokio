# debounce_on_tokio
A simple anti shake and throttling tool suitable for FLTK applications. 
Can be used to control the frequency of UI event processing or the frequency of function calls for other non UI events. 
Non macro processing, pure code control. Relying on tokio implementation.

Usage examples:
```rust
    let mut debounce_update = TokioDebounce::new_debounce(Box::new({
        let vp = version_package.clone();
        let mut script_editor = script_editor.clone();
        let tree = tree.clone();
        let editor_window = editor_window.clone();
        move |_| {
            // debug!("tree callback, {:?}", tree.first_selected_item());
            update_level_record(tree.clone(), vp.clone(), script_editor.clone(), editor_window.clone());
            update_tree_stat(tree.clone(), &mut script_editor);
        }
    }), Duration::from_millis(100), true);

    tree.set_callback({
        let mut script_editor = script_editor.clone();
        move |tree| {
            // debug!("tree callback, {:?}", tree.first_selected_item());

            if let Some(sel) = tree.first_selected_item() {
                let depth = sel.depth();
                if depth == 2 {
                    if let Err(_e) = script_editor.config_tabs.set_value(&script_editor.config_trigger_flex) {
                        // error!("切换触发器标签页时出错：{:?}", e);
                    }
                } else if depth == 3 {
                    if let Err(_e) = script_editor.config_tabs.set_value(&script_editor.config_expr_flex) {
                        // error!("切换触发器标签页时出错：{:?}", e);
                    }
                }
            }

            /*
            当鼠标点选某个树节点时，Changed事件会连续发生两次：第一次取消选择状态，第二次选中某个节点。
            因此这里需要做防抖处理，只处理有限时段内最后一次事件即可。
             */
            debounce_update.update_param(());
        }
    });
```