use patternfly_yew::prelude::*;
use std::rc::Rc;
use yew::prelude::*;

#[derive(PartialEq, Properties, Clone)]
struct SkeletonEntry {}

impl TableEntryRenderer<usize> for SkeletonEntry {
    fn render_cell(&self, _: CellContext<'_, usize>) -> Cell {
        html!(<Skeleton />).into()
    }
}

#[derive(Clone, PartialEq, Properties)]
pub struct SimpleTableProps<C, M>
where
    C: Clone + Eq + 'static,
    M: PartialEq + TableModel<C> + 'static,
{
    #[prop_or_default]
    pub loading: bool,

    #[prop_or_default]
    pub error: Option<String>,

    #[prop_or_default]
    pub header: Vec<TableColumnProperties<C>>,

    #[prop_or_default]
    pub empty: bool,

    #[prop_or_default]
    pub children: ChildrenWithProps<Table<C, M>>,
}

#[function_component(TableWrapper)]
pub fn table_wrapper<C, M>(props: &SimpleTableProps<C, M>) -> Html
where
    C: Clone + Eq + 'static,
    M: Clone + PartialEq + TableModel<C> + 'static,
{
    let header = html_nested!(
        <TableHeader<usize>>
            {
                for props.header.iter().enumerate()
                    .map(|(index, column)| {
                        yew::props!(TableColumnProperties<usize> {
                            index: index,
                            label: column.label.clone(),
                            width: column.width
                        })
                    })
                    .map(|column| html_nested!(<TableColumn<usize> ..column />))
            }
        </TableHeader<usize>>
    );

    let (empty_entries, _) = use_table_data(MemoizedTableModel::new(Rc::new(
        (0..0).map(|_| SkeletonEntry {}).collect(),
    )));
    let (skeleton_entries, _) = use_table_data(MemoizedTableModel::new(Rc::new(
        (0..10).map(|_| SkeletonEntry {}).collect(),
    )));

    // Loading view
    if props.loading {
        html!(
            <Table<usize, UseTableData<usize, MemoizedTableModel<SkeletonEntry>>>
                header={header}
                entries={skeleton_entries}
            />
        )
    } else if let Some(error) = &props.error {
        html!(
            <>
                <Table<usize, UseTableData<usize, MemoizedTableModel<SkeletonEntry>>>
                    header={header}
                    entries={empty_entries}
                />

                <Card>
                    <CardBody>
                        <Grid gutter=true>
                            <GridItem offset={[2]} cols={[2]}>
                                <img src="assets/images/chicken-svgrepo-com.svg" style="transform: scaleY(-1);"/>
                            </GridItem>
                            <GridItem cols={[6]}>
                                <Title>{"Error"}</Title>
                                { error }
                            </GridItem>
                        </Grid>
                    </CardBody>
                </Card>
            </>
        )
    } else if props.empty {
        html!(
            <>
                <Table<usize, UseTableData<usize, MemoizedTableModel<SkeletonEntry>>>
                    header={header}
                    entries={empty_entries}
                />
                <div style="background-color: var(--pf-v5-global--BackgroundColor--100);">
                    <EmptyState
                        title="No results"
                        icon={Icon::Search}
                        size={Size::Small}
                    >
                        { "Try a different search expression." }
                    </EmptyState>
                </div>
            </>
        )
    } else {
        html!(
            <>
                {
                    for props.children.iter().map(|mut item| {
                        let header = html_nested!(
                            <TableHeader<C>>
                                { for props.header.iter().map(|column| html_nested!(<TableColumn<C> ..column.clone() />)) }
                            </TableHeader<C>>
                        );

                        let item_props = Rc::make_mut(&mut item.props);
                        item_props.header = Some(header);
                        item
                    })
                }
            </>
        )
    }
}
