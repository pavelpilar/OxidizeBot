import React from "react";
import {Button, Alert, Table} from "react-bootstrap";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {Loading, Error} from 'shared-ui/components';

export default class BlockedSongs extends React.Component {
  constructor(props) {
    super(props);
    this.api = this.props.api;

    this.state = {
      loading: false,
      configLoading: false,
      error: null,
      data: null,
    };
  }

  async componentDidMount() {
    await this.list();
  }

  /**
   * Refresh the list of after streams.
   */
  async list() {
    this.setState({
      loading: true,
    });

    try {
      let data = await this.api.blockedSongs();
      console.log(data);
      this.setState({
        loading: false,
        error: null,
        data,
      });
    } catch(e) {
      this.setState({
        loading: false,
        error: `failed to request after streams: ${e}`,
        data: null,
      });
    }
  }

  /**
   * Delete the given afterstream.
   *
   * @param {number} id afterstream id to delete
   */
  async delete(id) {
    try {
      await this.api.deleteBlockedSong(id);
      await this.list();
    } catch(e) {
      this.setState({
        loading: false,
        error: `failed to delete blocked song: ${e}`,
      });
    }
  }

  render() {
    let content = null;
    console.log(this.state.data);

    if (this.state.data) {
      if (this.state.data.length === 0) {
        content = (
          <Alert variant="info">
            No Blocked Songs!
          </Alert>
        );
      } else {
        content = (
          <Table responsive="sm">
            <thead>
              <tr>
                <th>Track ID</th>
                <th className="table-fill">Blocked by</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {this.state.data.map((a, id) => {
                return (
                  <tr key={id}>
                    <td className="blockedsong-id">{a.track_id}</td>
                    <td><a className="blockedsong-user" href={`https://twitch.tv/${a.blocked_by}`}>@{a.blocked_by}</a></td>
                    <td>
                      <Button size="sm" variant="danger" className="action" onClick={() => this.delete(a.track_id)}>
                        <FontAwesomeIcon icon="trash" />
                      </Button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </Table>
        );
      }
    }

    return <>
      <h1 className='oxi-page-title'>Blocked Songs</h1>
      <Loading isLoading={this.state.loading || this.state.configLoading} />
      <Error error={this.state.error} />

      {content}
    </>;
  }
}